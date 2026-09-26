//! The actor phases of a tick (`docs/ACTORS.md` §1, §6):
//!
//! ```text
//! Think     par  every due actor runs its program against the tick-start world;
//!                writes its own mind and one Intent per actor into the chunk's Intents
//! Resolve   par  own chunk: sort intents by key, clear WAKE of every thinker, record
//!                in-chunk bites on the victim chunk's Scratch; cross-chunk bites -> Outbox
//! Exchange  seq  cross-chunk bites recorded on their victims; then, chunk by chunk in
//!                stage.active() order: bites per victim in key order, hurt + WAKE,
//!                deaths; food by share to the eaters; last every take/give in key order
//! Apply     par  own chunk: skip the dead, claim target cells (min key), apply die,
//!                become, drink, in-chunk move/spawn winners, look; result codes;
//!                cross-chunk move/spawn -> Outbox
//! Migrate   seq  every Outbox in stage.active() order, sorted by key: move/spawn into
//!                another chunk's cell if it is free and untouched this tick
//! Compact   par  swap-remove dead rows, repair occupant, reset scratch
//! ```
//!
//! Think reads any chunk's cells and public rows (nothing writes them in
//! this phase, so the live state is the tick-start snapshot) and writes only
//! its own chunk's minds and intents. Resolve, Apply and Compact touch only
//! the chunk they are handed; Exchange and Migrate are the sequential
//! phases, because they touch two chunks at once. Every order is sorted-key
//! order; every key is a function of state (`uid`, tick), never of a slot or
//! a thread. Every bite of a tick lands on a tick-start occupant, on either
//! side of a chunk border, before anyone moves. A cell whose occupancy
//! changed this tick (vacated, died, claimed, filled) is not enterable
//! until the next tick, so moves never chain.

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
    /// Operand kind of `Become` / `Spawn`; `1` for a validated `Drink`;
    /// own need slot of `Take` / `Give`.
    pub kind: u16,
    /// Operand offset of `Spawn` / `Move` (a unit step) / `Drink` / `Eat` / `Hit`.
    pub dx: i8,
    pub dy: i8,
    /// `look = v` effect, applied whatever the action's result.
    pub look: Option<u8>,
    /// `signal = v` effect, likewise.
    pub signal: Option<i16>,
    /// Amount of `Take` / `Give`.
    pub amount: i32,
    /// A `Spawn`'s child's first two `mem` values.
    pub with: [i32; 2],
    /// `mark ch v` effect: added to the actor's (tick-start) cell in Apply.
    pub mark: Option<(u8, u8)>,
    /// The think trapped (fuel or a fault) and was turned into `Idle`.
    pub trapped: bool,
    /// Ops the think executed (saturating), for the per-kind counters.
    pub used: u16,
}

/// The intents of one chunk's actors this tick. Scratch: cleared by Think,
/// consumed by Resolve and Apply.
#[derive(Component, Debug, Default)]
pub struct Intents {
    pub list: Vec<Intent>,
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
    /// The child's kind and first two `mem` values.
    Spawn {
        kind: u16,
        with: [i32; 2],
    },
    /// `take` (or `give`): up to `amount` of the mover's need `need` and
    /// the target's need of the same name. In-chunk ones too: every
    /// transfer is settled in Exchange, in key order.
    Transfer {
        need: u8,
        amount: i32,
        give: bool,
    },
    /// `eat` (true) or `hit`, `bite` damage, `dir` from the victim toward
    /// the biter (a `hurt_dir` value).
    Bite {
        bite: u8,
        eat: bool,
        dir: u8,
        cover: bool,
    },
}

/// Cross-chunk effects a chunk's actors asked for this tick: bites (filled
/// by Resolve, drained by Exchange), then moves and spawns (filled by
/// Apply, drained by Migrate).
#[derive(Component, Debug, Default)]
pub struct Outbox {
    pub list: Vec<Effect>,
}

/// A bite that landed on one of a chunk's actors this tick, from any chunk.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Hit {
    /// Victim's local cell (its tick-start position).
    pub cell: u16,
    pub key: u64,
    /// The biter: its chunk and row.
    pub from: Entity,
    pub slot: u16,
    pub bite: u8,
    pub eat: bool,
    pub dir: u8,
    /// Bites the ground cover (`graze`) rather than who stands there.
    pub cover: bool,
}

/// Working lists of the two sequential phases: every outbox of the tick,
/// sorted by key, and the food owed to eaters.
#[derive(Resource, Debug, Default)]
pub struct CrossScratch {
    list: Vec<(Entity, Effect)>,
    credits: Vec<(Entity, u16, i32)>,
}

/// Per-chunk resolve scratch: the claim table (`UNCLAIMED` = nobody wants
/// the cell and nothing changed there this tick, `TOUCHED` = its occupancy
/// changed this tick, else the lowest key that claimed it), the cells to
/// reset, the bites taken, how many rows died, and this tick's life events
/// per kind (merged into [`Tally`] at the end of the tick).
#[derive(Component, Debug)]
pub struct Scratch {
    claim: Vec<u64>,
    touched: Vec<u16>,
    pub hits: Vec<Hit>,
    pub deaths: u32,
    events: Vec<[u32; LIFE_EVENTS]>,
}

/// Life events and work counted per kind: `Tally::counts[kind][event]`.
pub mod life {
    /// A row created by `spawn`.
    pub const BORN: usize = 0;
    /// A row that became this kind (`become`: hatched, grown, sprouted).
    pub const BECAME: usize = 1;
    /// Killed by bites.
    pub const EATEN: usize = 2;
    /// Died of an empty vital need (or its own `die`).
    pub const DIED: usize = 3;
    /// Thinks run.
    pub const THINKS: usize = 4;
    /// Bytecode ops those thinks executed.
    pub const OPS: usize = 5;
    /// Thinks that trapped (fuel out, a second action, a fault) and idled.
    pub const TRAPS: usize = 6;
}
const LIFE_EVENTS: usize = 7;

/// Life events and work per kind since the world was loaded: births,
/// `become`s, deaths by bites and by needs; thinks, ops and traps. Sums, so
/// the order they are added in does not matter; not saved, not in the
/// checksum.
#[derive(Resource, Debug, Default, Clone, PartialEq, Eq)]
pub struct Tally {
    pub counts: Vec<[u64; LIFE_EVENTS]>,
}

impl Tally {
    pub fn get(&self, kind: u16, event: usize) -> u64 {
        self.counts.get(usize::from(kind)).map_or(0, |c| c[event])
    }
}

const UNCLAIMED: u64 = u64::MAX;
const TOUCHED: u64 = u64::MAX - 1;

impl Default for Scratch {
    fn default() -> Self {
        Self {
            claim: vec![UNCLAIMED; CHUNK_CELLS],
            touched: Vec::new(),
            hits: Vec::new(),
            deaths: 0,
            events: Vec::new(),
        }
    }
}

impl Scratch {
    /// `claim[cell] = min(claim[cell], key)`, unless the cell was touched.
    fn claim(&mut self, cell: usize, key: u64) {
        let c = &mut self.claim[cell];
        match *c {
            TOUCHED => {}
            UNCLAIMED => {
                self.touched.push(cell as u16);
                *c = key;
            }
            _ => *c = (*c).min(key),
        }
    }

    /// The cell's occupancy changed this tick: nobody else enters it.
    fn touch(&mut self, cell: usize) {
        if self.claim[cell] == UNCLAIMED {
            self.touched.push(cell as u16);
        }
        self.claim[cell] = TOUCHED;
    }

    /// Count a life event of `kind` this tick. The per-kind table grows on
    /// first use of a kind: chunk-level, not per actor.
    fn count(&mut self, kind: u16, event: usize) {
        self.add(kind, event, 1);
    }

    fn add(&mut self, kind: u16, event: usize, n: u32) {
        let k = usize::from(kind);
        if self.events.len() <= k {
            self.events.resize(k + 1, [0; LIFE_EVENTS]);
        }
        self.events[k][event] = self.events[k][event].saturating_add(n);
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
        self.hits.clear();
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
    kinds: &'a Kinds,
    c: ChunkCoord,
) -> Halo<'a> {
    let (tags, family_end) = (&kinds.tag_bits[..], &kinds.family_end[..]);
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
    Halo {
        chunks,
        tags,
        family_end,
    }
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
                let halo = halo.get_or_insert_with(|| halo_of(stage, cells, pubs, kinds, *coord));
                let mind = &mut minds.rows[slot];
                let (intent, _) = think_one::<false>(
                    kinds,
                    tick,
                    seed,
                    halo,
                    *coord,
                    slot as u16,
                    row,
                    mind,
                    &mut Vec::new(),
                );
                intents.list.push(intent);
            }
            if !intents.list.is_empty() {
                meta.dirty = true;
            }
        });
}

/// What one actor's think would do, for `wmc why`: its row, its mind
/// before (as stored) and after the think, the program's outcome, the
/// intent the resolve phases would get, and every op it executed.
#[derive(Debug, Clone)]
pub struct Explained {
    pub coord: ChunkCoord,
    pub slot: u16,
    pub row: ActorPub,
    pub before: ActorMind,
    pub after: ActorMind,
    /// Thinks at this tick by cadence (or a wake-up).
    pub due: bool,
    /// A vital need was empty: the think was `die` without running rules.
    pub starved: bool,
    pub outcome: vm::Outcome,
    pub intent: Intent,
    pub trace: Vec<vm::Step>,
}

/// Run one actor's think on a copy of its mind, with a trace. Pure: the
/// same function the Think phase runs, against the same tick-start state.
#[allow(clippy::too_many_arguments)]
pub fn explain(
    kinds: &Kinds,
    tick: u64,
    seed: u64,
    halo: &Halo<'_>,
    coord: ChunkCoord,
    slot: u16,
    row: &ActorPub,
    before: &ActorMind,
) -> Explained {
    let mut after = *before;
    let mut trace = Vec::new();
    let (intent, outcome) = think_one::<true>(
        kinds, tick, seed, halo, coord, slot, row, &mut after, &mut trace,
    );
    Explained {
        coord,
        slot,
        row: *row,
        before: *before,
        after,
        due: due(tick, row, kinds.def(row.kind).cadence()),
        starved: intent.action == Action::Die && trace.is_empty(),
        outcome,
        intent,
        trace,
    }
}

/// One actor's think: decay, die at zero, else run the program and turn
/// its outcome into an intent. Events read by this think are cleared
/// afterwards; a trap sets `FUEL` for the next. With `TRACE`, every op the
/// program executes goes to `trace`.
#[allow(clippy::too_many_arguments)]
#[inline(always)]
fn think_one<const TRACE: bool>(
    kinds: &Kinds,
    tick: u64,
    seed: u64,
    halo: &Halo<'_>,
    coord: ChunkCoord,
    slot: u16,
    row: &ActorPub,
    mind: &mut ActorMind,
    trace: &mut Vec<vm::Step>,
) -> (Intent, vm::Outcome) {
    let kind = kinds.def(row.kind);
    let key = intent_key(mind.uid, tick);
    let mut intent = Intent {
        slot,
        key,
        action: Action::Idle,
        kind: 0,
        dx: 0,
        dy: 0,
        look: None,
        signal: None,
        amount: 0,
        with: [0; 2],
        mark: None,
        trapped: false,
        used: 0,
    };
    if vm::decay(kind, mind, tick) {
        clear_events(mind);
        intent.action = Action::Die;
        return (intent, vm::Outcome::default());
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
    let out = if TRACE {
        vm::think_traced(kinds, ctx, mind, trace)
    } else {
        vm::think(kinds, ctx, mind)
    };
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
    intent.look = out.look;
    intent.signal = out.signal;
    intent.amount = out.amount;
    intent.with = out.with;
    intent.mark = out.mark;
    if matches!(out.action, Action::Take | Action::Give) {
        intent.kind = u16::from(out.need);
    }
    intent.trapped = out.trap.is_some();
    intent.used = u16::try_from(out.used).unwrap_or(u16::MAX);
    let (lx, ly) = local_xy(cell);
    match out.action {
        Action::Move => {
            let (dx, dy) = step_toward(halo, lx, ly, i32::from(out.dx), i32::from(out.dy));
            intent.dx = dx as i8;
            intent.dy = dy as i8;
        }
        Action::Drink => {
            // Adjacent water (or underfoot, which a standing actor never is).
            let (dx, dy) = (i32::from(out.dx), i32::from(out.dy));
            let water = pred::ground(Ground::Water as u8);
            let valid = dx.abs() <= 1 && dy.abs() <= 1 && halo.matches(lx, ly, dx, dy, water);
            intent.kind = u16::from(valid);
        }
        _ => {}
    }
    (intent, out)
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

/// Reduce a move to one step: `(sign dx, sign dy)`, and if that cell is not
/// free at tick start, the 45-degree neighbour clockwise, then the one
/// counter-clockwise. None free: the straight step, which Apply will
/// report as BLOCKED. `(0, 0)` stays `(0, 0)`.
pub fn step_toward(halo: &Halo<'_>, lx: i32, ly: i32, dx: i32, dy: i32) -> (i32, i32) {
    let s = (dx.signum(), dy.signum());
    if s == (0, 0) {
        return s;
    }
    let dirs = &vm::DIRS8;
    let i = dirs.iter().position(|&d| d == s).expect("a unit step");
    for j in [i, (i + 1) % 8, (i + 7) % 8] {
        let (cx, cy) = dirs[j];
        if halo.free(lx, ly, cx, cy) {
            return (cx, cy);
        }
    }
    s
}

// ---- Resolve ----------------------------------------------------------------------------

/// Own chunk, in parallel: intents into key order, WAKE consumed by every
/// actor that thought, bites recorded. A bite must be adjacent and land on
/// an actor with a `health` need (else REFUSED); an empty cell is MISSED.
/// Bites into another chunk go to the Outbox for Exchange.
pub fn resolve(
    kinds: Res<Kinds>,
    mut q: Query<(
        Entity,
        &ChunkCoord,
        &ChunkCells,
        &mut ChunkActors,
        &mut ChunkMinds,
        &mut Intents,
        &mut Scratch,
        &mut Outbox,
    )>,
) {
    let kinds = &*kinds;
    q.par_iter_mut().for_each(
        |(e, coord, cells, mut pubs, mut minds, mut intents, mut scratch, mut outbox)| {
            outbox.list.clear();
            if intents.list.is_empty() {
                return;
            }
            // Every order below is a function of state, never of slot history.
            intents.list.sort_unstable_by_key(|i| (i.key, i.slot));
            for it in &intents.list {
                let slot = usize::from(it.slot);
                // The think consumed the wake-up; a bite this tick sets it again.
                pubs.rows[slot].flags &= !flags::WAKE;
                if matches!(it.action, Action::Take | Action::Give) {
                    let (dx, dy) = (i32::from(it.dx), i32::from(it.dy));
                    let res = if dx.abs() > 1 || dy.abs() > 1 || (dx, dy) == (0, 0) {
                        result::REFUSED
                    } else {
                        let (to, cell) = match target_of(
                            *coord,
                            usize::from(pubs.rows[slot].cell),
                            it.dx,
                            it.dy,
                        ) {
                            Where::Here(cell) => (*coord, cell),
                            Where::Elsewhere(to, cell) => (to, cell),
                        };
                        outbox.list.push(Effect {
                            key: it.key,
                            slot: it.slot,
                            what: EffectKind::Transfer {
                                need: it.kind as u8,
                                amount: it.amount,
                                give: it.action == Action::Give,
                            },
                            to,
                            cell: cell as u16,
                        });
                        result::NONE // Exchange decides
                    };
                    set_result_in(&mut minds, slot, res);
                    continue;
                }
                if !matches!(it.action, Action::Eat | Action::Hit | Action::Graze) {
                    continue;
                }
                let (dx, dy) = (i32::from(it.dx), i32::from(it.dy));
                // `eat`/`hit` bite a neighbour; `graze` the cover next to or
                // under the grazer.
                let cover = it.action == Action::Graze;
                let res = if dx.abs() > 1 || dy.abs() > 1 || ((dx, dy) == (0, 0) && !cover) {
                    result::REFUSED
                } else {
                    let row = pubs.rows[slot];
                    let bite = kinds.def(row.kind).bite;
                    let eat = it.action != Action::Hit;
                    let dir = vm::dir_index(-dx, -dy);
                    match target_of(*coord, usize::from(row.cell), it.dx, it.dy) {
                        Where::Here(cell) => match layer_of(cells, cover)[cell].unpack() {
                            None => result::MISSED,
                            Some((vk, _)) if !has_health(kinds, vk) => result::REFUSED,
                            Some(_) => {
                                scratch.hits.push(Hit {
                                    cell: cell as u16,
                                    key: it.key,
                                    from: e,
                                    slot: it.slot,
                                    bite,
                                    eat,
                                    dir,
                                    cover,
                                });
                                result::OK
                            }
                        },
                        Where::Elsewhere(to, cell) => {
                            outbox.list.push(Effect {
                                key: it.key,
                                slot: it.slot,
                                what: EffectKind::Bite {
                                    bite,
                                    eat,
                                    dir,
                                    cover,
                                },
                                to,
                                cell: cell as u16,
                            });
                            result::NONE // Exchange decides
                        }
                    }
                };
                set_result_in(&mut minds, slot, res);
            }
        },
    );
}

#[inline]
fn has_health(kinds: &Kinds, kind: u16) -> bool {
    kinds.def(kind).need_named("health").is_some()
}

/// The occupant layer, or the cover layer.
#[inline]
fn layer_of(cells: &ChunkCells, cover: bool) -> &[ActorId; CHUNK_CELLS] {
    if cover { &cells.cover } else { &cells.occupant }
}

// ---- Exchange ---------------------------------------------------------------------------

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

/// Damage, deaths and feeding, one thread. First every cross-chunk bite is
/// recorded on its victim (tick-start occupancy: nothing has died or moved
/// yet). Then, chunk by chunk in `stage.active()` order, the bites on each
/// victim take its `health` in key order, each at most what is left; its
/// `hurt` grows by what was taken (saturating), `hurt_dir` points at the
/// lowest-key biter and `WAKE` is set; at `health <= 0` it dies. Every `eat`
/// gains the share of the victim kind's `food` it took (`food * taken /
/// max health`), if the eater is alive itself at the end: a fox eats a
/// chicken over two bites, a chicken crops grass that grows back.
pub fn exchange(
    kinds: Res<Kinds>,
    stage: Res<Stage>,
    mut work: ResMut<CrossScratch>,
    mut outboxes: Query<&mut Outbox>,
    mut chunks: ChunkQuery,
) {
    let kinds = &*kinds;
    let work = &mut *work;
    work.list.clear();
    work.credits.clear();
    for &(_, e) in stage.active() {
        if let Ok(mut ob) = outboxes.get_mut(e)
            && !ob.list.is_empty()
        {
            work.list.extend(ob.list.drain(..).map(|fx| (e, fx)));
        }
    }
    work.list.sort_unstable_by_key(|(_, fx)| (fx.key, fx.slot));
    for &(src_e, fx) in &work.list {
        let EffectKind::Bite {
            bite,
            eat,
            dir,
            cover,
        } = fx.what
        else {
            continue;
        };
        let res = match stage.entity(fx.to).map(|e| chunks.get_mut(e)) {
            Some(Ok((cells, _, _, mut scratch, _))) => {
                match layer_of(&cells, cover)[usize::from(fx.cell)].unpack() {
                    None => result::MISSED,
                    Some((vk, _)) if !has_health(kinds, vk) => result::REFUSED,
                    Some(_) => {
                        scratch.hits.push(Hit {
                            cell: fx.cell,
                            key: fx.key,
                            from: src_e,
                            slot: fx.slot,
                            bite,
                            eat,
                            dir,
                            cover,
                        });
                        result::OK
                    }
                }
            }
            // Not loaded: nobody there.
            _ => result::MISSED,
        };
        set_result(&mut chunks, src_e, usize::from(fx.slot), res);
    }

    for &(_, e) in stage.active() {
        let Ok((mut cells, mut pubs, mut minds, mut scratch, mut meta)) = chunks.get_mut(e) else {
            continue;
        };
        if scratch.hits.is_empty() {
            continue;
        }
        let scratch = &mut *scratch;
        scratch
            .hits
            .sort_unstable_by_key(|h| (h.cover, h.cell, h.key, h.slot));
        let mut i = 0;
        while i < scratch.hits.len() {
            let first = scratch.hits[i];
            let mut j = i;
            while j < scratch.hits.len()
                && (scratch.hits[j].cell, scratch.hits[j].cover) == (first.cell, first.cover)
            {
                j += 1;
            }
            let group = i..j;
            i = j;
            let cell = usize::from(first.cell);
            let Some((vk, vslot)) = layer_of(&cells, first.cover)[cell].unpack() else {
                continue;
            };
            let vslot = usize::from(vslot);
            let def = kinds.def(vk);
            let Some(h) = def.need_named("health") else {
                continue;
            };
            // Bites in key order, each taking what is left of the victim's
            // health; an `eat` feeds its biter the share of the victim's
            // `food` it took (`food * taken / max health`), so overkill
            // feeds nobody and a shared kill is shared.
            let max = i64::from(def.needs[h].max.max(1));
            let m = &mut minds.rows[vslot];
            let mut left = m.needs[h].max(0);
            for hit in &scratch.hits[group] {
                let taken = i32::from(hit.bite).min(left);
                left -= taken;
                if hit.eat && taken > 0 && def.food > 0 {
                    let food = i64::from(def.food) * i64::from(taken) / max;
                    work.credits.push((hit.from, hit.slot, food as i32));
                }
            }
            let taken = m.needs[h].max(0) - left;
            m.needs[h] = left;
            m.hurt = m.hurt.saturating_add(taken.clamp(0, 255) as u8);
            m.hurt_dir = first.dir;
            let dead = left <= 0;
            pubs.rows[vslot].flags |= flags::WAKE;
            if dead {
                ActorsMut {
                    pubs: &mut pubs.rows,
                    minds: &mut minds.rows,
                    cells: &mut cells,
                }
                .kill(vslot);
                scratch.deaths += 1;
                if !first.cover {
                    scratch.touch(cell); // a grazed-out tuft frees no standing room
                }
                scratch.count(vk, life::EATEN);
            }
            meta.dirty = true;
        }
    }

    for &(e, slot, food) in &work.credits {
        let Ok((_, pubs, mut minds, _, _)) = chunks.get_mut(e) else {
            continue;
        };
        let row = pubs.rows[usize::from(slot)];
        if row.flags & flags::DEAD != 0 {
            continue;
        }
        let def = kinds.def(row.kind);
        if let Some(f) = def.need_named("food") {
            let m = &mut minds.rows[usize::from(slot)];
            m.needs[f] = m.needs[f].saturating_add(food).min(def.needs[f].max);
        }
    }

    // Transfers last, in key order, after every death of the tick.
    for &(src_e, fx) in &work.list {
        let EffectKind::Transfer { need, amount, give } = fx.what else {
            continue;
        };
        let slot = usize::from(fx.slot);
        let t = Transfer {
            need: usize::from(need),
            amount,
            give,
        };
        if let Some(res) = transfer(kinds, &stage, &mut chunks, src_e, slot, &fx, t) {
            set_result(&mut chunks, src_e, slot, res);
        }
    }
}

/// One `take`/`give`, decoded.
#[derive(Debug, Clone, Copy)]
struct Transfer {
    need: usize,
    amount: i32,
    give: bool,
}

/// Settle one transfer between the mover (`src_e`, `slot`) and whoever
/// stands on the target cell at tick start. `take` moves up to `amount` of
/// the target's same-named need into the mover's, `give` the reverse, never
/// more than the source holds or past the receiver's max. The target of a
/// `take` gets `TAKEN` and wakes. `None` if the mover died this tick (its
/// intent is void); else the mover's result: MISSED (nobody there, or dead
/// now), REFUSED (the target has no such need), OK.
fn transfer(
    kinds: &Kinds,
    stage: &Stage,
    chunks: &mut ChunkQuery,
    src_e: Entity,
    slot: usize,
    fx: &Effect,
    t: Transfer,
) -> Option<u8> {
    let Ok((_, src_pubs, src_minds, _, _)) = chunks.get(src_e) else {
        return None;
    };
    let src = src_pubs.rows[slot];
    if src.flags & flags::DEAD != 0 {
        return None;
    }
    let sv = src_minds.rows[slot].needs[t.need];
    let Some(dst_e) = stage.entity(fx.to) else {
        return Some(result::MISSED);
    };
    let Ok((cells, dst_pubs, dst_minds, _, _)) = chunks.get(dst_e) else {
        return Some(result::MISSED);
    };
    let Some((dk, ds)) = cells.occupant[usize::from(fx.cell)].unpack() else {
        return Some(result::MISSED);
    };
    let ds = usize::from(ds);
    if dst_pubs.rows[ds].flags & flags::DEAD != 0 {
        return Some(result::MISSED);
    }
    let sdef = kinds.def(src.kind);
    let ddef = kinds.def(dk);
    let Some(dn) = ddef.need_named(&sdef.needs[t.need].name) else {
        return Some(result::REFUSED);
    };
    let dv = dst_minds.rows[ds].needs[dn];
    let (smax, dmax) = (sdef.needs[t.need].max, ddef.needs[dn].max);
    let moved = if t.give {
        t.amount.min(sv.max(0)).min((dmax - dv).max(0))
    } else {
        t.amount.min(dv.max(0)).min((smax - sv).max(0))
    };
    let (sv, dv) = if t.give {
        (sv - moved, dv + moved)
    } else {
        (sv + moved, dv - moved)
    };
    if let Ok((_, _, mut minds, _, mut meta)) = chunks.get_mut(src_e) {
        minds.rows[slot].needs[t.need] = sv;
        meta.dirty = true;
    }
    if let Ok((_, mut pubs, mut minds, _, mut meta)) = chunks.get_mut(dst_e) {
        let m = &mut minds.rows[ds];
        m.needs[dn] = dv;
        if !t.give {
            m.events |= event::TAKEN;
            pubs.rows[ds].flags |= flags::WAKE;
        }
        meta.dirty = true;
    }
    Some(result::OK)
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
        &Intents,
        &mut Scratch,
        &mut Outbox,
    )>,
) {
    let (tick, seed, kinds) = (tick.0, cfg.seed, &*kinds);
    q.par_iter_mut().for_each(
        |(coord, mut cells, mut pubs, mut minds, intents, mut scratch, mut outbox)| {
            // Exchange drained the bites; what goes in now is moves and spawns.
            outbox.list.clear();
            if intents.list.is_empty() {
                return;
            }
            // Claims: in-chunk targets free at tick start and untouched, lowest
            // key wins. Intents are in key order since Resolve.
            for it in &intents.list {
                if !matches!(it.action, Action::Move | Action::Spawn) {
                    continue;
                }
                if it.action == Action::Spawn && is_cover(kinds, it.kind) {
                    continue; // ground cover takes no standing room
                }
                let row = pubs.rows[usize::from(it.slot)];
                if row.flags & flags::DEAD != 0 {
                    continue;
                }
                if let Some(cell) = local_target(usize::from(row.cell), it.dx, it.dy)
                    && cells.walkable(cell)
                    && cells.occupant[cell].is_none()
                {
                    scratch.claim(cell, it.key);
                }
            }

            for it in &intents.list {
                let slot = usize::from(it.slot);
                let kind = pubs.rows[slot].kind;
                scratch.count(kind, life::THINKS);
                scratch.add(kind, life::OPS, u32::from(it.used));
                if it.trapped {
                    scratch.count(kind, life::TRAPS);
                }
                if pubs.rows[slot].flags & flags::DEAD != 0 {
                    continue; // died in Exchange: its intent is void
                }
                let from = usize::from(pubs.rows[slot].cell);
                let grounded = pubs.rows[slot].flags & flags::COVER != 0;
                let mut actors = ActorsMut {
                    pubs: &mut pubs.rows,
                    minds: &mut minds.rows,
                    cells: &mut cells,
                };
                let res = match it.action {
                    Action::Idle => Some(result::OK),
                    // Resolve and Exchange already wrote the result.
                    Action::Eat | Action::Hit | Action::Graze | Action::Take | Action::Give => None,
                    Action::Die => {
                        let kind = actors.pubs[slot].kind;
                        actors.kill(slot);
                        scratch.deaths += 1;
                        if !grounded {
                            scratch.touch(from);
                        }
                        scratch.count(kind, life::DIED);
                        Some(result::OK)
                    }
                    Action::Become => {
                        let res = change_kind(kinds, tick, &mut actors, slot, it.kind);
                        if res == result::OK {
                            scratch.count(it.kind, life::BECAME);
                        }
                        Some(res)
                    }
                    Action::Drink => {
                        let kind = kinds.def(actors.pubs[slot].kind);
                        Some(match (it.kind, kind.need_named("water")) {
                            (1, Some(i)) => {
                                actors.minds[slot].needs[i] = kind.needs[i].max;
                                result::OK
                            }
                            _ => result::REFUSED,
                        })
                    }
                    // Ground cover is rooted.
                    Action::Move if grounded => Some(result::REFUSED),
                    Action::Move => Some(match target_of(*coord, from, it.dx, it.dy) {
                        Where::Here(cell) if scratch.claim[cell] == it.key => {
                            let kind = actors.pubs[slot].kind;
                            actors.cells.occupant[from] = ActorId::NONE;
                            scratch.touch(from);
                            actors.cells.occupant[cell] = ActorId::pack(kind, it.slot);
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
                    }),
                    Action::Spawn if usize::from(it.kind) >= kinds.len() => Some(result::REFUSED),
                    // Ground cover: the first spawn in key order onto a
                    // walkable cell with no cover gets it.
                    Action::Spawn if is_cover(kinds, it.kind) => {
                        Some(match target_of(*coord, from, it.dx, it.dy) {
                            Where::Here(cell)
                                if actors.cells.walkable(cell)
                                    && actors.cells.cover[cell].is_none() =>
                            {
                                let pos = coord.cell(cell);
                                let mut child = newborn(
                                    kinds,
                                    it.kind,
                                    hash_cell(seed, STREAM_UID, pos.x, pos.y) ^ splitmix64(tick),
                                    tick,
                                );
                                child.mem[..2].copy_from_slice(&it.with);
                                actors.push_cover(cell, it.kind, child);
                                scratch.count(it.kind, life::BORN);
                                result::OK
                            }
                            Where::Here(_) => result::BLOCKED,
                            Where::Elsewhere(to, cell) => {
                                outbox.list.push(Effect {
                                    key: it.key,
                                    slot: it.slot,
                                    what: EffectKind::Spawn {
                                        kind: it.kind,
                                        with: it.with,
                                    },
                                    to,
                                    cell: cell as u16,
                                });
                                result::NONE
                            }
                        })
                    }
                    Action::Spawn => Some(match target_of(*coord, from, it.dx, it.dy) {
                        Where::Here(cell) if scratch.claim[cell] == it.key => {
                            let pos = coord.cell(cell);
                            let mut child = newborn(
                                kinds,
                                it.kind,
                                hash_cell(seed, STREAM_UID, pos.x, pos.y) ^ splitmix64(tick),
                                tick,
                            );
                            child.mem[..2].copy_from_slice(&it.with);
                            actors.push(cell, it.kind, child);
                            scratch.touch(cell);
                            scratch.count(it.kind, life::BORN);
                            result::OK
                        }
                        Where::Here(_) => result::BLOCKED,
                        Where::Elsewhere(to, cell) => {
                            outbox.list.push(Effect {
                                key: it.key,
                                slot: it.slot,
                                what: EffectKind::Spawn {
                                    kind: it.kind,
                                    with: it.with,
                                },
                                to,
                                cell: cell as u16,
                            });
                            result::NONE
                        }
                    }),
                };
                if let Some(res) = res {
                    set_result_in(&mut minds, slot, res);
                }
                if let Some(look) = it.look {
                    pubs.rows[slot].look = look;
                }
                if let Some(signal) = it.signal {
                    pubs.rows[slot].signal = signal;
                }
                if let Some((ch, v)) = it.mark {
                    let s = &mut cells.scent[usize::from(ch)][from];
                    *s = s.saturating_add(v);
                }
            }
        },
    );
}

#[inline]
fn is_cover(kinds: &Kinds, kind: u16) -> bool {
    kinds.def(kind).cover
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
    let grounded = actors.pubs[slot].flags & flags::COVER != 0;
    if kinds.def(to).cover != grounded {
        return result::REFUSED; // a row cannot change layers
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
    actors.pubs[slot].kind = to;
    let cell = usize::from(actors.pubs[slot].cell);
    actors.layer(grounded)[cell] = ActorId::pack(to, slot as u16);
    result::OK
}

// ---- Migrate ----------------------------------------------------------------------------

/// Cross-chunk moves and spawns, one thread, every outbox in `stage.active()`
/// order and then every effect in key order. A target cell must be loaded,
/// walkable, empty and untouched this tick; an in-chunk winner from Apply
/// therefore always beats a cross-chunk one (home advantage, decision 30).
pub fn migrate(
    tick: Res<Tick>,
    cfg: Res<SimConfig>,
    kinds: Res<Kinds>,
    stage: Res<Stage>,
    mut work: ResMut<CrossScratch>,
    mut outboxes: Query<&mut Outbox>,
    mut chunks: ChunkQuery,
) {
    let (tick, seed) = (tick.0, cfg.seed);
    work.list.clear();
    for &(_, e) in stage.active() {
        if let Ok(mut ob) = outboxes.get_mut(e)
            && !ob.list.is_empty()
        {
            work.list.extend(ob.list.drain(..).map(|fx| (e, fx)));
        }
    }
    if work.list.is_empty() {
        return;
    }
    work.list.sort_unstable_by_key(|(_, fx)| (fx.key, fx.slot));
    for &(src_e, fx) in &work.list {
        if matches!(
            fx.what,
            EffectKind::Bite { .. } | EffectKind::Transfer { .. }
        ) {
            continue; // Exchange's; never here
        }
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
        let cover = matches!(fx.what, EffectKind::Spawn { kind, .. } if is_cover(&kinds, kind));
        let ok = dst_cells.walkable(cell)
            && if cover {
                dst_cells.cover[cell].is_none()
            } else {
                dst_cells.occupant[cell].is_none() && dst_scratch.enterable(cell)
            };
        if !ok {
            set_result_in(&mut src.2, slot, result::BLOCKED);
            continue;
        }
        let (src_cells, src_pubs, src_minds, src_scratch, src_meta) = &mut src;
        let mut to = ActorsMut {
            pubs: &mut dst_pubs.rows,
            minds: &mut dst_minds.rows,
            cells: dst_cells,
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
                moved.flags = row.flags & flags::WAKE;
                let mut from = ActorsMut {
                    pubs: &mut src_pubs.rows,
                    minds: &mut src_minds.rows,
                    cells: src_cells,
                };
                from.kill(slot);
                src_scratch.deaths += 1;
                src_scratch.touch(usize::from(row.cell));
            }
            EffectKind::Spawn { kind, with } => {
                let pos = fx.to.cell(cell);
                let mut child = newborn(
                    &kinds,
                    kind,
                    hash_cell(seed, STREAM_UID, pos.x, pos.y) ^ splitmix64(tick),
                    tick,
                );
                child.mem[..2].copy_from_slice(&with);
                if cover {
                    to.push_cover(cell, kind, child);
                } else {
                    to.push(cell, kind, child);
                }
                dst_scratch.count(kind, life::BORN);
                set_result_in(src_minds, slot, result::OK);
            }
            EffectKind::Bite { .. } | EffectKind::Transfer { .. } => {
                unreachable!("skipped above")
            }
        }
        if !cover {
            dst_scratch.touch(cell);
        }
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

// ---- Tally ------------------------------------------------------------------------------

/// Fold every chunk's life events of the tick into [`Tally`]. Sums commute,
/// so chunk order does not matter.
pub fn tally(mut total: ResMut<Tally>, mut q: Query<&mut Scratch>) {
    for mut s in &mut q {
        if s.events.iter().all(|e| e.iter().all(|&n| n == 0)) {
            continue;
        }
        let s = &mut *s;
        if total.counts.len() < s.events.len() {
            total.counts.resize(s.events.len(), [0; LIFE_EVENTS]);
        }
        for (t, e) in total.counts.iter_mut().zip(s.events.iter_mut()) {
            for (a, b) in t.iter_mut().zip(e.iter_mut()) {
                *a += u64::from(*b);
                *b = 0;
            }
        }
    }
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
                cells: &mut cells,
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
        Halo {
            chunks,
            tags: &[],
            family_end: &[],
        }
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
    fn claims_take_the_lowest_key_and_never_a_touched_cell() {
        let mut s = Scratch::default();
        s.claim(5, 30);
        s.claim(5, 10);
        s.claim(5, 20);
        assert_eq!(s.claim[5], 10);
        s.touch(6);
        s.claim(6, 1);
        assert_eq!(s.claim[6], TOUCHED);
        assert!(!s.enterable(5) && !s.enterable(6) && s.enterable(7));
        s.hits.push(Hit {
            cell: 1,
            key: 0,
            from: Entity::PLACEHOLDER,
            slot: 0,
            bite: 1,
            eat: false,
            dir: 0,
            cover: false,
        });
        s.reset();
        assert!(s.enterable(5) && s.enterable(6) && s.hits.is_empty());
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
        assert_eq!(a.cells.occupant[5], ActorId::pack(TREE, 0));
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
        assert_eq!(vm::dir_index(0, -1), 1);
        assert_eq!(vm::dir_index(-1, -1), 8);
        assert_eq!(vm::dir_index(0, 0), 0);
    }
}
