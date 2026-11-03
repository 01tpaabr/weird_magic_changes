//! The actor phases of a tick (`docs/ACTORS.md` §1, §6):
//!
//! ```text
//! Think     par  every due actor runs its program against the tick-start world;
//!                writes its own mind and one Intent per actor into the chunk's Intents
//! Resolve   par  own chunk: sort intents by key, clear WAKE of every thinker, record
//!                in-chunk bites on the victim chunk's Scratch; cross-chunk bites and every
//!                take/give -> Outbox
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

use std::collections::BTreeMap;

use bevy_ecs::prelude::*;

use crate::actors::{
    ActorMind, ActorPub, ActorsMut, ChunkActors, ChunkMinds, MEM_SLOTS, NEED_SLOTS, flags, slot_u16,
};
use crate::rng::{hash_cell, splitmix64};
use crate::rules::vm::{self, Action, Ctx, Halo, event, result};
use crate::rules::{KindDef, Kinds, Policy, Remap};
use crate::sim::{SimConfig, Tick};
use crate::stage::worldgen::{STREAM_UID, STREAM_UID_COVER, STREAM_UID_LAYER};
use crate::stage::{
    ActorId, CHUNK_CELLS, ChunkCells, ChunkCoord, ChunkMeta, Feature, Ground, Pos, Stage, cell_u16,
};
use crate::time::stamp;

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
    /// `grow n` effects, added up: heads a group gains (or loses), likewise.
    pub grow: i32,
    /// Amount of `Take` / `Give`; heads a `Split` takes.
    pub amount: i32,
    /// A `Spawn`'s child's first two `mem` values.
    pub with: [i32; 2],
    /// Field of `Harvest` / `Drop` (whose own need slot is in `kind`).
    pub field: u8,
    /// `add f v` effects: the fields (`vm::NO_FIELD`: unused) and what to
    /// add to each on the actor's (tick-start) cell in Apply. Two arrays,
    /// not pairs: no padding in a row every think writes.
    pub add_fields: [u8; vm::ADDS],
    pub add_values: [i32; vm::ADDS],
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
    /// `build`: the material (a ground, or `0x100 | feature`).
    Build {
        material: u16,
    },
    /// `dig`: clear the feature.
    Dig,
    /// `split`: a new group of the mover's kind with `heads` of its heads.
    Split {
        heads: u16,
    },
    /// `join`: the mover's heads go to the group of its kind there.
    Join,
    /// `take` (or `give`): up to `amount` of the mover's need `need` and
    /// the target's need of the same name. In-chunk ones too: every
    /// transfer is settled in Exchange, in key order.
    Transfer {
        need: u8,
        amount: i32,
        give: bool,
    },
    /// `eat` (true) or `hit`, `bite` damage, `dir` from the victim toward
    /// the biter (a `hurt_dir` value), and the biter's kind (whose bite
    /// killed, for [`Tally::deaths`]).
    Bite {
        bite: u8,
        eat: bool,
        dir: u8,
        cover: bool,
        kind: u16,
    },
}

/// Effects a chunk's actors asked for this tick: bites that cross a chunk and
/// every take/give (filled by Resolve, drained by Exchange), then cross-chunk
/// moves and spawns (filled by Apply, drained by Migrate).
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
    /// The biter's kind.
    pub kind: u16,
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
    /// Claims in the declared layers, `(layer, cell, key)`, sorted: the
    /// first of a `(layer, cell)` wins it.
    layer_claims: Vec<(u8, u16, u64)>,
    pub hits: Vec<Hit>,
    pub deaths: u32,
    events: Vec<[u32; LIFE_EVENTS]>,
    /// This tick's deaths, by kind and cause, each with how many heads
    /// died (a group loses one or more): few, so a list.
    causes: Vec<(u16, Cause, u32)>,
    /// Per player, with a `budget { rows N }`: its rows here and its spawns
    /// wanted here ([`ration`] fills them), then the spawns this chunk may
    /// make this tick (Apply spends them in key order).
    players: Vec<(u32, u32)>,
    quota: Vec<u32>,
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

/// Why a row died: [`life::EATEN`] and [`life::DIED`] split by cause.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Cause {
    /// A bite took its last health: the biter's kind.
    Killed(u16),
    /// The vital need in this slot of its kind was empty at its think.
    Starved(u8),
    /// One of its rules chose `die` (old age, say).
    Died,
    /// Its layer rests on a kind (`layer NAME { on KIND }`) and none stood
    /// under it at the end of a tick.
    Fell,
}

/// Life events and work per kind since the world was loaded: births,
/// `become`s, deaths by bites and by needs; thinks, ops and traps; and
/// every death by kind and cause. Sums, so the order they are added in does
/// not matter; not saved, not in the checksum.
#[derive(Resource, Debug, Default, Clone, PartialEq, Eq)]
pub struct Tally {
    pub counts: Vec<[u64; LIFE_EVENTS]>,
    /// Deaths per (kind, cause).
    pub deaths: BTreeMap<(u16, Cause), u64>,
}

impl Tally {
    pub fn get(&self, kind: u16, event: usize) -> u64 {
        self.counts.get(usize::from(kind)).map_or(0, |c| c[event])
    }

    /// The deaths of `kind` so far, by cause.
    pub fn deaths_of(&self, kind: u16) -> impl Iterator<Item = (Cause, u64)> + '_ {
        self.deaths
            .range((kind, Cause::Killed(0))..=(kind, Cause::Fell))
            .map(|(&(_, c), &n)| (c, n))
    }
}

const UNCLAIMED: u64 = u64::MAX;
const TOUCHED: u64 = u64::MAX - 1;

impl Default for Scratch {
    fn default() -> Self {
        Self {
            claim: vec![UNCLAIMED; CHUNK_CELLS],
            touched: Vec::new(),
            layer_claims: Vec::new(),
            hits: Vec::new(),
            deaths: 0,
            events: Vec::new(),
            causes: Vec::new(),
            players: Vec::new(),
            quota: Vec::new(),
        }
    }
}

impl Scratch {
    /// May a spawn of `kind` go ahead under the arena's budget? A player's
    /// spends one of its chunk's spawns this tick; there are none left: no.
    fn ration(&mut self, kinds: &Kinds, kind: u16) -> bool {
        if kinds.players.rows.is_none() {
            return true;
        }
        let Some(q) = kinds.players.of(kind).and_then(|p| self.quota.get_mut(p)) else {
            return true;
        };
        if *q == 0 {
            return false;
        }
        *q -= 1;
        true
    }

    /// Does `key` hold the lowest claim on `cell` of declared layer `layer`?
    fn layer_won(&self, layer: u8, cell: usize, key: u64) -> bool {
        let at = self
            .layer_claims
            .partition_point(|&(l, c, _)| (l, usize::from(c)) < (layer, cell));
        self.layer_claims
            .get(at)
            .is_some_and(|&(l, c, k)| (l, usize::from(c), k) == (layer, cell, key))
    }

    /// `claim[cell] = min(claim[cell], key)`, unless the cell was touched.
    fn claim(&mut self, cell: usize, key: u64) {
        let c = &mut self.claim[cell];
        match *c {
            TOUCHED => {}
            UNCLAIMED => {
                self.touched.push(cell_u16(cell));
                *c = key;
            }
            _ => *c = (*c).min(key),
        }
    }

    /// The cell's occupancy changed this tick: nobody else enters it.
    fn touch(&mut self, cell: usize) {
        if self.claim[cell] == UNCLAIMED {
            self.touched.push(cell_u16(cell));
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

/// Is an actor due to think at `tick`? Above a cadence of 2^16 the 16-bit
/// stagger is shifted up, so the phases still spread over the whole period.
#[inline]
pub fn due(tick: u64, row: &ActorPub, cadence: u64) -> bool {
    let shift = cadence.trailing_zeros().saturating_sub(16);
    row.flags & flags::WAKE != 0
        || tick.wrapping_add(u64::from(row.stagger) << shift) & (cadence - 1) == 0
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
                let halo = halo.get_or_insert_with(|| {
                    Halo::around(*coord, kinds, |c| {
                        let e = stage.entity(c)?;
                        Some((cells.get(e).ok()?, pubs.get(e).ok()?))
                    })
                });
                let mind = &mut minds.rows[slot];
                let (intent, _) = think_one::<false>(
                    kinds,
                    tick,
                    seed,
                    halo,
                    *coord,
                    slot_u16(slot),
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
/// same function the Think phase runs, against the halo it is given
/// (`sim::explain_slot` builds the one Think sees).
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
        grow: 0,
        amount: 0,
        with: [0; 2],
        field: 0,
        add_fields: [vm::NO_FIELD; vm::ADDS],
        add_values: [0; vm::ADDS],
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
        heads: row.heads,
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
    intent.grow = out.grow;
    intent.amount = out.amount;
    intent.with = out.with;
    intent.field = out.field;
    intent.add_fields = out.adds.map(|(f, _)| f);
    intent.add_values = out.adds.map(|(_, v)| v);
    if matches!(
        out.action,
        Action::Take | Action::Give | Action::Harvest | Action::Drop
    ) {
        intent.kind = u16::from(out.need);
    }
    if out.action == Action::Split {
        intent.kind = row.kind; // the new group's: claimed as a spawn's
    }
    intent.trapped = out.trap.is_some();
    intent.used = u16::try_from(out.used).unwrap_or(u16::MAX);
    let (lx, ly) = (vm::lx(cell), vm::ly(cell));
    match out.action {
        Action::Move if kind.is_big() => {
            let (dx, dy) = step_fitting(halo, lx, ly, i32::from(out.dx), i32::from(out.dy), kind);
            #[allow(clippy::cast_possible_truncation)] // a unit step, -1..=1
            let step = (dx as i8, dy as i8);
            (intent.dx, intent.dy) = step;
        }
        Action::Move => {
            let (dx, dy) = step_toward_climbing(
                halo,
                lx,
                ly,
                i32::from(out.dx),
                i32::from(out.dy),
                kind.climb,
                kind.layer,
            );
            #[allow(clippy::cast_possible_truncation)] // a unit step, -1..=1
            let step = (dx as i8, dy as i8);
            (intent.dx, intent.dy) = step;
        }
        Action::Drink => {
            // An adjacent ground to drink from (or underfoot, which a
            // standing actor seldom is: a drinkable ground is seldom walkable).
            let (dx, dy) = (i32::from(out.dx), i32::from(out.dy));
            let drinkable = halo
                .at(lx, ly, dx, dy)
                .is_some_and(|(c, _, i)| halo.materials.drinkable(c.ground[i]));
            let valid = dx.abs() <= 1 && dy.abs() <= 1 && drinkable;
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

/// Reduce a move to one step: `(sign dx, sign dy)`, and if that cell is not
/// free at tick start, the 45-degree neighbour clockwise, then the one
/// counter-clockwise. None free: the straight step, which Apply will
/// report as BLOCKED. `(0, 0)` stays `(0, 0)`.
pub fn step_toward(halo: &Halo<'_>, lx: i32, ly: i32, dx: i32, dy: i32) -> (i32, i32) {
    step_toward_climbing(halo, lx, ly, dx, dy, None, 0)
}

/// [`step_toward`] for a kind that climbs at most `n` in field `f`
/// (`climb n on f`): a step that rises more is not free, and if every one is
/// too steep the step is `(0, 0)`, which Apply reports as BLOCKED.
pub fn step_toward_climbing(
    halo: &Halo<'_>,
    lx: i32,
    ly: i32,
    dx: i32,
    dy: i32,
    climb: Option<(u8, i32)>,
    layer: u8,
) -> (i32, i32) {
    let s = (dx.signum(), dy.signum());
    if s == (0, 0) {
        return s;
    }
    let gentle = |cx: i32, cy: i32| {
        climb.is_none_or(|(f, n)| {
            let f = usize::from(f);
            halo.field(lx, ly, cx, cy, f) - halo.field(lx, ly, 0, 0, f) <= n
        })
    };
    let dirs = &vm::DIRS8;
    let i = dirs.iter().position(|&d| d == s).expect("a unit step");
    for j in [i, (i + 1) % 8, (i + 7) % 8] {
        let (cx, cy) = dirs[j];
        if halo.test(lx, ly, cx, cy, halo.free_of(layer)) && gentle(cx, cy) {
            return (cx, cy);
        }
    }
    if gentle(s.0, s.1) { s } else { (0, 0) }
}

/// [`step_toward_climbing`] for a big actor (`size W H`): a step its whole
/// footprint fits into at tick start ([`Halo::fits`]), straight, else 45
/// degrees either way; none: the straight step, which Migrate reports as
/// BLOCKED.
fn step_fitting(halo: &Halo<'_>, lx: i32, ly: i32, dx: i32, dy: i32, kind: &KindDef) -> (i32, i32) {
    let s = (dx.signum(), dy.signum());
    if s == (0, 0) {
        return s;
    }
    let gentle = |cx: i32, cy: i32| {
        kind.climb.is_none_or(|(f, n)| {
            let f = usize::from(f);
            halo.field(lx, ly, cx, cy, f) - halo.field(lx, ly, 0, 0, f) <= n
        })
    };
    let dirs = &vm::DIRS8;
    let i = dirs.iter().position(|&d| d == s).expect("a unit step");
    for j in [i, (i + 1) % 8, (i + 7) % 8] {
        let (cx, cy) = dirs[j];
        if halo.fits(lx, ly, cx, cy, kind) && gentle(cx, cy) {
            return (cx, cy);
        }
    }
    s
}

// ---- Resolve ----------------------------------------------------------------------------

/// Own chunk, in parallel: intents into key order, WAKE consumed by every
/// actor that thought, bites recorded. A bite must be within the biter's `reach` (a neighbour by default) and land on
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
                        // A take or give's kind is its need slot (Think
                        // widened the u8).
                        #[allow(clippy::cast_possible_truncation)]
                        let need = it.kind as u8;
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
                                need,
                                amount: it.amount,
                                give: it.action == Action::Give,
                            },
                            to,
                            cell: cell_u16(cell),
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
                // `eat`/`hit` bite a neighbour, or from a declared layer the
                // one standing under the biter; `graze` the cover next to or
                // under the grazer.
                let cover = it.action == Action::Graze;
                let reach = i32::from(kinds.def(pubs.rows[slot].kind).reach);
                let under = cover || pubs.rows[slot].layer >= 2;
                let res = if dx.abs() > reach || dy.abs() > reach || ((dx, dy) == (0, 0) && !under)
                {
                    result::REFUSED
                } else {
                    let row = pubs.rows[slot];
                    let bite = kinds.def(row.kind).bite;
                    let eat = it.action != Action::Hit;
                    // From afar too: the direction the bite came from.
                    let dir = vm::dir_index((-dx).signum(), (-dy).signum());
                    // A part of a big actor: the bite goes to its anchor,
                    // here or (through Exchange) in another chunk.
                    let at = match target_of(*coord, usize::from(row.cell), it.dx, it.dy) {
                        Where::Here(cell) => match cells.layer(u8::from(cover))[cell].part_of() {
                            Some((_, ox, oy)) => {
                                let p = coord.cell(cell);
                                let (to, c) = Pos::new(p.x - ox, p.y - oy).split();
                                if to == *coord {
                                    Where::Here(c)
                                } else {
                                    Where::Elsewhere(to, c)
                                }
                            }
                            None => Where::Here(cell),
                        },
                        w => w,
                    };
                    match at {
                        Where::Here(cell) => match cells.layer(u8::from(cover))[cell].row() {
                            None => result::MISSED,
                            Some((vk, _)) if !has_health(kinds, vk) => result::REFUSED,
                            Some(_) => {
                                scratch.hits.push(Hit {
                                    cell: cell_u16(cell),
                                    key: it.key,
                                    from: e,
                                    slot: it.slot,
                                    bite,
                                    eat,
                                    dir,
                                    cover,
                                    kind: row.kind,
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
                                    kind: row.kind,
                                },
                                to,
                                cell: cell_u16(cell),
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
/// victim take its `health` (decayed to now) in key order, each at most
/// what is left; its `hurt` grows by what was taken (saturating), `hurt_dir`
/// points at the lowest-key biter and `WAKE` is set; at `health <= 0` it dies. Every `eat`
/// gains the share of the victim kind's `food` it took (`food * taken /
/// max health`), if the eater is alive itself at the end: a fox eats a
/// chicken over two bites, a chicken crops grass that grows back.
pub fn exchange(
    tick: Res<Tick>,
    kinds: Res<Kinds>,
    stage: Res<Stage>,
    mut work: ResMut<CrossScratch>,
    mut outboxes: Query<&mut Outbox>,
    mut chunks: ChunkQuery,
) {
    let (tick, kinds) = (tick.0, &*kinds);
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
            kind,
        } = fx.what
        else {
            continue;
        };
        // A part of a big actor: the bite lands on its anchor.
        let (to, cell) = anchor_in(
            &stage,
            &chunks,
            fx.to,
            usize::from(fx.cell),
            u8::from(cover),
        );
        let res = match stage.entity(to).map(|e| chunks.get_mut(e)) {
            Some(Ok((cells, _, _, mut scratch, _))) => {
                match cells.layer(u8::from(cover))[cell].row() {
                    None => result::MISSED,
                    Some((vk, _)) if !has_health(kinds, vk) => result::REFUSED,
                    Some(_) => {
                        scratch.hits.push(Hit {
                            cell: cell_u16(cell),
                            key: fx.key,
                            from: src_e,
                            slot: fx.slot,
                            bite,
                            eat,
                            dir,
                            cover,
                            kind,
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
            let Some((vk, vslot)) = cells.layer(u8::from(first.cover))[cell].row() else {
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
            if def.needs[h].decays {
                vm::decay(def, m, tick); // health as it stands now
            }
            let mut left = m.needs[h].max(0);
            // Whose bite took the last of its health (the first biter's, if
            // it had none left).
            let mut killer = first.kind;
            // A group's heads each have the health: a bite that takes the
            // last of one goes on into the next, at full health, so one
            // bite may take several; the last head's is the row's death.
            let mut heads = pubs.rows[vslot].heads.max(1);
            let (mut lost, mut taken_all) = (0u16, 0i32);
            for hit in &scratch.hits[group] {
                let mut bite = i32::from(hit.bite);
                loop {
                    let taken = bite.min(left);
                    left -= taken;
                    bite -= taken;
                    taken_all = taken_all.saturating_add(taken);
                    if hit.eat && taken > 0 && def.food > 0 {
                        // Health is in 0..=max (every write clamps it), so
                        // taken <= max and food <= def.food, an i32.
                        #[allow(clippy::cast_possible_truncation)]
                        let food = (i64::from(def.food) * i64::from(taken) / max) as i32;
                        work.credits.push((hit.from, hit.slot, food));
                    }
                    if left > 0 || heads == 0 {
                        break;
                    }
                    if taken > 0 || lost == 0 {
                        killer = hit.kind;
                    }
                    heads -= 1;
                    lost += 1;
                    if heads == 0 || bite == 0 {
                        if heads > 0 {
                            left = def.needs[h].max;
                        }
                        break;
                    }
                    left = def.needs[h].max;
                }
            }
            m.needs[h] = left;
            m.hurt = m.hurt.saturating_add(taken_all.clamp(0, 255) as u8);
            m.hurt_dir = first.dir;
            let dead = heads == 0;
            pubs.rows[vslot].flags |= flags::WAKE;
            pubs.rows[vslot].heads = heads.max(1);
            if lost > 0 && !dead {
                scratch.add(vk, life::EATEN, u32::from(lost));
                scratch
                    .causes
                    .push((vk, Cause::Killed(killer), u32::from(lost)));
            }
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
                scratch.add(vk, life::EATEN, u32::from(lost));
                scratch
                    .causes
                    .push((vk, Cause::Killed(killer), u32::from(lost)));
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
        if let Some(res) = transfer(kinds, &stage, tick, &mut chunks, src_e, slot, &fx, t) {
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
/// more than the source holds or past the receiver's max. A decaying need
/// of the target is first decayed to now. The target of a `take` that
/// moved anything gets `TAKEN` and wakes. `None` if the mover died this tick (its
/// intent is void); else the mover's result: MISSED (nobody there, or dead
/// now), REFUSED (the target has no such need), BLOCKED (nothing moved), OK.
fn transfer(
    kinds: &Kinds,
    stage: &Stage,
    tick: u64,
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
    let (to, cell) = anchor_in(stage, chunks, fx.to, usize::from(fx.cell), 0);
    let Some(dst_e) = stage.entity(to) else {
        return Some(result::MISSED);
    };
    let Ok((cells, mut dst_pubs, mut dst_minds, _, mut dst_meta)) = chunks.get_mut(dst_e) else {
        return Some(result::MISSED);
    };
    let Some((dk, ds)) = cells.occupant[cell].row() else {
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
    let m = &mut dst_minds.rows[ds];
    if ddef.needs[dn].decays {
        // The target last thought a while ago: bring it to now first. An
        // emptied vital need kills it at its next think.
        vm::decay(ddef, m, tick);
        dst_meta.dirty = true;
    }
    let dv = m.needs[dn];
    let (smax, dmax) = (sdef.needs[t.need].max, ddef.needs[dn].max);
    let moved = if t.give {
        t.amount.min(sv.max(0)).min((dmax - dv).max(0))
    } else {
        t.amount.min(dv.max(0)).min((smax - sv).max(0))
    };
    if moved == 0 {
        return Some(result::BLOCKED); // nothing moved: nobody saw a take
    }
    let (sv, dv) = if t.give {
        (sv - moved, dv + moved)
    } else {
        (sv + moved, dv - moved)
    };
    m.needs[dn] = dv;
    if !t.give {
        m.events |= event::TAKEN;
        dst_pubs.rows[ds].flags |= flags::WAKE;
    }
    dst_meta.dirty = true;
    if let Ok((_, _, mut minds, _, mut meta)) = chunks.get_mut(src_e) {
        minds.rows[slot].needs[t.need] = sv;
        meta.dirty = true;
    }
    Some(result::OK)
}

/// Where the actor at `cell` of chunk `to`, in `layer`, keeps its row: the
/// same cell, or for a part of a big actor its anchor, maybe in another
/// chunk. As it stands now; a chunk not loaded is taken as it is.
fn anchor_in(
    stage: &Stage,
    chunks: &ChunkQuery,
    to: ChunkCoord,
    cell: usize,
    layer: u8,
) -> (ChunkCoord, usize) {
    let part = stage
        .entity(to)
        .and_then(|e| chunks.get(e).ok())
        .and_then(|(cells, ..)| cells.layer(layer)[cell].part_of());
    match part {
        Some((_, ox, oy)) => {
            let p = to.cell(cell);
            Pos::new(p.x - ox, p.y - oy).split()
        }
        None => (to, cell),
    }
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
            // key wins. Intents are in key order since Resolve; a winner
            // touches the cell, so a tied key (twin uids) after it is blocked.
            scratch.layer_claims.clear();
            for it in &intents.list {
                if !matches!(
                    it.action,
                    Action::Move | Action::Spawn | Action::Build | Action::Split
                ) {
                    continue;
                }
                if it.action == Action::Spawn && is_cover(kinds, it.kind) {
                    continue; // ground cover takes no standing room
                }
                let row = pubs.rows[usize::from(it.slot)];
                if row.flags & flags::DEAD != 0 {
                    continue;
                }
                // A big actor's move or birth is Migrate's: it claims nothing here.
                let big = match it.action {
                    Action::Move => kinds.def(row.kind).is_big(),
                    Action::Spawn => kinds.def(it.kind).is_big(),
                    _ => false,
                };
                if big {
                    continue;
                }
                // A declared layer: its own claims, a slot empty at tick
                // start, any ground if it flies.
                let layer = match it.action {
                    Action::Move | Action::Split => row.layer,
                    Action::Spawn => kinds.def(it.kind).layer,
                    _ => 0,
                };
                if layer >= 2 {
                    if let Some(cell) = local_target(usize::from(row.cell), it.dx, it.dy)
                        && cells.layer(layer)[cell].is_none()
                        && (kinds.flies(layer) || cells.walkable(cell, &kinds.materials.table))
                        && kinds
                            .rest(layer)
                            .holds(cells.occupant[cell], &kinds.tag_bits)
                    {
                        scratch.layer_claims.push((layer, cell_u16(cell), it.key));
                    }
                    continue;
                }
                if it.action == Action::Move && row.flags & flags::COVER != 0 {
                    continue; // REFUSED below: a rooted row claims nothing
                }
                if let Some(cell) = local_target(usize::from(row.cell), it.dx, it.dy)
                    && cells.walkable(cell, &kinds.materials.table)
                    && cells.occupant[cell].is_none()
                {
                    scratch.claim(cell, it.key);
                }
            }

            scratch.layer_claims.sort_unstable();
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
                let cover = pubs.rows[slot].flags & flags::COVER != 0;
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
                        // Think stamped the needs to now: an empty vital
                        // one made this `die`, else a rule chose it.
                        let mind = &actors.minds[slot];
                        let cause = kinds
                            .def(kind)
                            .needs
                            .iter()
                            .zip(mind.needs)
                            .position(|(n, v)| n.vital && v <= 0)
                            .map_or(Cause::Died, |i| {
                                Cause::Starved(u8::try_from(i).expect("4 need slots"))
                            });
                        // A group whose vital need ran out loses one head,
                        // and the rest go on with it refilled; a rule's
                        // `die` ends the whole group.
                        let heads = actors.pubs[slot].heads.max(1);
                        if heads > 1 && cause != Cause::Died {
                            actors.pubs[slot].heads = heads - 1;
                            let def = kinds.def(kind);
                            let m = &mut actors.minds[slot];
                            for (v, n) in m.needs.iter_mut().zip(&def.needs) {
                                if n.vital && *v <= 0 {
                                    *v = n.max;
                                }
                            }
                            scratch.count(kind, life::DIED);
                            scratch.causes.push((kind, cause, 1));
                            Some(result::OK)
                        } else {
                            actors.kill(slot);
                            scratch.deaths += 1;
                            if !cover {
                                scratch.touch(from);
                            }
                            scratch.add(kind, life::DIED, u32::from(heads));
                            scratch.causes.push((kind, cause, u32::from(heads)));
                            Some(result::OK)
                        }
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
                    Action::Harvest | Action::Drop => {
                        Some(harvest(kinds, &mut actors, slot, from, it))
                    }
                    Action::Build | Action::Dig => {
                        let near = it.dx.abs() <= 1 && it.dy.abs() <= 1;
                        let build = it.action == Action::Build;
                        Some(match target_of(*coord, from, it.dx, it.dy) {
                            _ if !near || (build && it.dx == 0 && it.dy == 0) => result::REFUSED,
                            Where::Here(cell) if build => {
                                let claimed = scratch.claim[cell] == it.key;
                                if claimed && lay(kinds, actors.cells, cell, it.kind) {
                                    scratch.touch(cell);
                                    result::OK
                                } else {
                                    result::BLOCKED
                                }
                            }
                            Where::Here(cell) => {
                                if actors.cells.feature[cell] == Feature::None {
                                    result::BLOCKED
                                } else {
                                    actors.cells.feature[cell] = Feature::None;
                                    scratch.touch(cell);
                                    result::OK
                                }
                            }
                            Where::Elsewhere(to, cell) => {
                                outbox.list.push(Effect {
                                    key: it.key,
                                    slot: it.slot,
                                    what: if build {
                                        EffectKind::Build { material: it.kind }
                                    } else {
                                        EffectKind::Dig
                                    },
                                    to,
                                    cell: cell_u16(cell),
                                });
                                result::NONE
                            }
                        })
                    }
                    // A big actor's move: every cell of its footprint,
                    // maybe in other chunks, so Migrate settles it.
                    Action::Move if kinds.def(actors.pubs[slot].kind).is_big() => {
                        let (to, cell) = match target_of(*coord, from, it.dx, it.dy) {
                            Where::Here(cell) => (*coord, cell),
                            Where::Elsewhere(to, cell) => (to, cell),
                        };
                        outbox.list.push(Effect {
                            key: it.key,
                            slot: it.slot,
                            what: EffectKind::Move,
                            to,
                            cell: cell_u16(cell),
                        });
                        Some(result::NONE)
                    }
                    // Ground cover is rooted.
                    Action::Move if cover => Some(result::REFUSED),
                    Action::Move if actors.pubs[slot].layer >= 2 => {
                        let l = actors.pubs[slot].layer;
                        Some(match target_of(*coord, from, it.dx, it.dy) {
                            // Still walkable: a `build` settled before it may
                            // have changed that.
                            Where::Here(cell)
                                if scratch.layer_won(l, cell, it.key)
                                    && (kinds.flies(l)
                                        || actors.cells.walkable(cell, &kinds.materials.table))
                                    && kinds
                                        .rest(l)
                                        .holds(actors.cells.occupant[cell], &kinds.tag_bits) =>
                            {
                                let kind = actors.pubs[slot].kind;
                                actors.cells.layer_mut(l)[from] = ActorId::NONE;
                                actors.cells.layer_mut(l)[cell] = ActorId::pack(kind, it.slot);
                                actors.pubs[slot].cell = cell_u16(cell);
                                result::OK
                            }
                            Where::Here(_) => result::BLOCKED,
                            Where::Elsewhere(to, cell) => {
                                outbox.list.push(Effect {
                                    key: it.key,
                                    slot: it.slot,
                                    what: EffectKind::Move,
                                    to,
                                    cell: cell_u16(cell),
                                });
                                result::NONE
                            }
                        })
                    }
                    Action::Move => Some(match target_of(*coord, from, it.dx, it.dy) {
                        Where::Here(cell) if scratch.claim[cell] == it.key => {
                            let kind = actors.pubs[slot].kind;
                            actors.cells.occupant[from] = ActorId::NONE;
                            scratch.touch(from);
                            actors.cells.occupant[cell] = ActorId::pack(kind, it.slot);
                            actors.pubs[slot].cell = cell_u16(cell);
                            scratch.touch(cell);
                            result::OK
                        }
                        Where::Here(_) => result::BLOCKED,
                        Where::Elsewhere(to, cell) => {
                            outbox.list.push(Effect {
                                key: it.key,
                                slot: it.slot,
                                what: EffectKind::Move,
                                to,
                                cell: cell_u16(cell),
                            });
                            result::NONE // Migrate decides
                        }
                    }),
                    Action::Split => Some(split(
                        kinds,
                        seed,
                        tick,
                        *coord,
                        &mut scratch,
                        &mut outbox,
                        &mut actors,
                        it,
                    )),
                    Action::Join => Some(join(
                        kinds,
                        &mut scratch,
                        &mut outbox,
                        &mut actors,
                        *coord,
                        it,
                    )),
                    // The arena's budget: this player has no room left.
                    Action::Spawn if !scratch.ration(kinds, it.kind) => Some(result::REFUSED),
                    // Past the halo, even into a loaded chunk: as unloaded.
                    Action::Spawn if beyond(from, it.dx, it.dy) => Some(result::BLOCKED),
                    // A big child: Migrate places its footprint.
                    Action::Spawn if kinds.def(it.kind).is_big() => {
                        let (to, cell) = match target_of(*coord, from, it.dx, it.dy) {
                            Where::Here(cell) => (*coord, cell),
                            Where::Elsewhere(to, cell) => (to, cell),
                        };
                        outbox.list.push(Effect {
                            key: it.key,
                            slot: it.slot,
                            what: EffectKind::Spawn {
                                kind: it.kind,
                                with: it.with,
                            },
                            to,
                            cell: cell_u16(cell),
                        });
                        Some(result::NONE)
                    }
                    Action::Spawn if kinds.def(it.kind).layer >= 2 => {
                        let l = kinds.def(it.kind).layer;
                        Some(match target_of(*coord, from, it.dx, it.dy) {
                            Where::Here(cell)
                                if scratch.layer_won(l, cell, it.key)
                                    && (kinds.flies(l)
                                        || actors.cells.walkable(cell, &kinds.materials.table))
                                    && kinds
                                        .rest(l)
                                        .holds(actors.cells.occupant[cell], &kinds.tag_bits) =>
                            {
                                let child = spawn_mind(
                                    kinds,
                                    seed,
                                    tick,
                                    coord.cell(cell),
                                    it.kind,
                                    it.with,
                                );
                                actors.push_in(cell, it.kind, child, l);
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
                                    cell: cell_u16(cell),
                                });
                                result::NONE
                            }
                        })
                    }
                    // Ground cover: the first spawn in key order onto a
                    // walkable cell with no cover gets it.
                    Action::Spawn if is_cover(kinds, it.kind) => {
                        Some(match target_of(*coord, from, it.dx, it.dy) {
                            Where::Here(cell)
                                if actors.cells.walkable(cell, &kinds.materials.table)
                                    && actors.cells.cover[cell].is_none() =>
                            {
                                let child = spawn_mind(
                                    kinds,
                                    seed,
                                    tick,
                                    coord.cell(cell),
                                    it.kind,
                                    it.with,
                                );
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
                                    cell: cell_u16(cell),
                                });
                                result::NONE
                            }
                        })
                    }
                    Action::Spawn => Some(match target_of(*coord, from, it.dx, it.dy) {
                        Where::Here(cell) if scratch.claim[cell] == it.key => {
                            let child =
                                spawn_mind(kinds, seed, tick, coord.cell(cell), it.kind, it.with);
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
                                cell: cell_u16(cell),
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
                if it.grow != 0 && pubs.rows[slot].flags & flags::DEAD == 0 {
                    grow(
                        kinds,
                        &mut cells,
                        &mut pubs,
                        &mut minds,
                        &mut scratch,
                        slot,
                        it.grow,
                    );
                }
                for (&f, &v) in it.add_fields.iter().zip(&it.add_values) {
                    let f = usize::from(f);
                    if let (Some(def), Some(layer)) = (kinds.fields.get(f), cells.fields.get_mut(f))
                        && def.policy == Policy::Free
                    {
                        let c = &mut layer[from];
                        *c = clamp_field(i32::from(*c).saturating_add(v), def.max);
                    }
                }
            }
        },
    );
}

/// `split n at t`: `n` of a group's heads, at least one and fewer than it
/// has, start a new group of its kind on the adjacent cell `t`, claimed as
/// a spawn's (the arena's budget counts it as one). The new group starts
/// as the old one is: needs, memory, state, age. Across a chunk border,
/// Migrate settles it.
#[allow(clippy::too_many_arguments)]
fn split(
    kinds: &Kinds,
    seed: u64,
    tick: u64,
    coord: ChunkCoord,
    scratch: &mut Scratch,
    outbox: &mut Outbox,
    actors: &mut ActorsMut<'_>,
    it: &Intent,
) -> u8 {
    let slot = usize::from(it.slot);
    let row = actors.pubs[slot];
    let near = it.dx.abs() <= 1 && it.dy.abs() <= 1 && (it.dx, it.dy) != (0, 0);
    let Ok(heads) = u16::try_from(it.amount) else {
        return result::REFUSED;
    };
    if !near || heads == 0 || heads >= row.heads || !scratch.ration(kinds, row.kind) {
        return result::REFUSED;
    }
    let l = row.layer;
    match target_of(coord, usize::from(row.cell), it.dx, it.dy) {
        Where::Here(cell) => {
            let won = if l >= 2 {
                scratch.layer_won(l, cell, it.key)
                    && (kinds.flies(l) || actors.cells.walkable(cell, &kinds.materials.table))
                    && kinds
                        .rest(l)
                        .holds(actors.cells.occupant[cell], &kinds.tag_bits)
            } else {
                scratch.claim[cell] == it.key
            };
            if !won {
                return result::BLOCKED;
            }
            let child = split_mind(
                kinds,
                seed,
                tick,
                coord.cell(cell),
                row.kind,
                &actors.minds[slot],
            );
            let new = actors.push_in(cell, row.kind, child, l);
            actors.pubs[usize::from(new)].heads = heads;
            actors.pubs[slot].heads -= heads;
            if l == 0 {
                scratch.touch(cell);
            }
            result::OK
        }
        Where::Elsewhere(to, cell) => {
            outbox.list.push(Effect {
                key: it.key,
                slot: it.slot,
                what: EffectKind::Split { heads },
                to,
                cell: cell_u16(cell),
            });
            result::NONE
        }
    }
}

/// `join t`: a group gives all its heads to the group of its kind on the
/// adjacent cell `t` (in its layer), if they fit under the kind's `group
/// max`, and is gone. The one joined keeps its own needs and memory.
/// Across a chunk border, Migrate settles it.
fn join(
    kinds: &Kinds,
    scratch: &mut Scratch,
    outbox: &mut Outbox,
    actors: &mut ActorsMut<'_>,
    coord: ChunkCoord,
    it: &Intent,
) -> u8 {
    let slot = usize::from(it.slot);
    let row = actors.pubs[slot];
    let near = it.dx.abs() <= 1 && it.dy.abs() <= 1 && (it.dx, it.dy) != (0, 0);
    if !near {
        return result::REFUSED;
    }
    match target_of(coord, usize::from(row.cell), it.dx, it.dy) {
        Where::Here(cell) => {
            let Some((k, t)) = actors.cells.layer(row.layer)[cell].row() else {
                return result::MISSED;
            };
            let t = usize::from(t);
            if k != row.kind || actors.pubs[t].flags & flags::DEAD != 0 {
                return result::MISSED;
            }
            let sum = u32::from(actors.pubs[t].heads) + u32::from(row.heads);
            if sum > u32::from(kinds.def(k).max_heads()) {
                return result::REFUSED;
            }
            actors.pubs[t].heads = u16::try_from(sum).expect("at most the kind's max");
            actors.kill(slot);
            scratch.deaths += 1;
            if row.layer == 0 {
                scratch.touch(usize::from(row.cell));
            }
            result::OK
        }
        Where::Elsewhere(to, cell) => {
            outbox.list.push(Effect {
                key: it.key,
                slot: it.slot,
                what: EffectKind::Join,
                to,
                cell: cell_u16(cell),
            });
            result::NONE
        }
    }
}

/// `grow n`: a group's heads change by `n`, clamped to 0 and its kind's
/// `group max`; heads gained are births, heads lost deaths (`Cause::Died`),
/// and a group with none left is gone. Nothing for a kind that is not a
/// group.
fn grow(
    kinds: &Kinds,
    cells: &mut ChunkCells,
    pubs: &mut ChunkActors,
    minds: &mut ChunkMinds,
    scratch: &mut Scratch,
    slot: usize,
    n: i32,
) {
    let row = pubs.rows[slot];
    let def = kinds.def(row.kind);
    if def.group.0 == 0 {
        return;
    }
    let new = (i32::from(row.heads).saturating_add(n)).clamp(0, i32::from(def.max_heads()));
    let delta = new - i32::from(row.heads);
    if delta > 0 {
        scratch.add(row.kind, life::BORN, delta.unsigned_abs());
    } else if delta < 0 {
        scratch.add(row.kind, life::DIED, delta.unsigned_abs());
        scratch
            .causes
            .push((row.kind, Cause::Died, delta.unsigned_abs()));
    }
    if new == 0 {
        ActorsMut {
            pubs: &mut pubs.rows,
            minds: &mut minds.rows,
            cells,
        }
        .kill(slot);
        scratch.deaths += 1;
        if row.layer == 0 {
            scratch.touch(usize::from(row.cell));
        }
    } else {
        pubs.rows[slot].heads = u16::try_from(new).expect("1..=max, clamped above");
    }
}

/// The mind of a group `split` off another onto `pos` this tick: the
/// parent's, but its own uid (derived as a spawn's: a cell takes one birth
/// a layer a tick) and no events.
fn split_mind(
    kinds: &Kinds,
    seed: u64,
    tick: u64,
    pos: Pos,
    kind: u16,
    parent: &ActorMind,
) -> ActorMind {
    let uid = spawn_mind(kinds, seed, tick, pos, kind, [0; 2]).uid;
    ActorMind {
        uid,
        events: 0,
        hurt: 0,
        hurt_dir: 0,
        ..*parent
    }
}

/// Lay `material` (a ground, or `0x100 | feature`) on `cell`, which the
/// builder claimed (walkable and empty at tick start). A feature only where
/// there is none; whatever leaves the cell unwalkable only where no ground
/// cover lies. `false` if it may not, or the material is not the rules'.
fn lay(kinds: &Kinds, cells: &mut ChunkCells, cell: usize, material: u16) -> bool {
    let m = &kinds.materials;
    let (g, f) = if material >= 0x100 {
        let f = u8::try_from(material - 0x100).unwrap_or(u8::MAX);
        if f == 0 || usize::from(f) > m.features.len() || cells.feature[cell] != Feature::None {
            return false;
        }
        (cells.ground[cell], Feature(f))
    } else {
        let g = u8::try_from(material).unwrap_or(u8::MAX);
        if usize::from(g) >= m.grounds.len() {
            return false;
        }
        (Ground(g), cells.feature[cell])
    };
    // Nobody is left on a cell they can't stand on: the cover, and anyone
    // in a declared layer that does not fly.
    if !m.table.walkable(g, f)
        && (!cells.cover[cell].is_none()
            || (2u8..)
                .zip(&cells.extra)
                .any(|(l, a)| !a[cell].is_none() && !kinds.flies(l)))
    {
        return false;
    }
    (cells.ground[cell], cells.feature[cell]) = (g, f);
    true
}

/// A value clamped into a field's `0..=max`.
#[inline]
fn clamp_field(v: i32, max: u16) -> u16 {
    u16::try_from(v.clamp(0, i32::from(max))).expect("clamped into 0..=u16::MAX")
}

/// Settle a `harvest` (field into need) or `drop` (need into field) on the
/// actor's own cell `from`: up to the amount, never more than the source
/// holds or past the receiver's max. The need was stamped to now by this
/// tick's think. REFUSED for a field the rules don't have or a `fixed` one
/// (the compiler refuses both); BLOCKED if nothing moved; else OK.
fn harvest(kinds: &Kinds, actors: &mut ActorsMut<'_>, slot: usize, from: usize, it: &Intent) -> u8 {
    let f = usize::from(it.field);
    let (Some(def), Some(layer)) = (kinds.fields.get(f), actors.cells.fields.get_mut(f)) else {
        return result::REFUSED;
    };
    let kind = kinds.def(actors.pubs[slot].kind);
    let n = usize::from(it.kind);
    let Some(need) = kind.needs.get(n) else {
        return result::REFUSED;
    };
    if def.policy == Policy::Fixed {
        return result::REFUSED;
    }
    let own = &mut actors.minds[slot].needs[n];
    let cell = i32::from(layer[from]);
    let moved = if it.action == Action::Harvest {
        it.amount.min(cell).min(need.max - *own)
    } else {
        it.amount.min(*own).min(i32::from(def.max) - cell)
    }
    .max(0);
    if moved == 0 {
        return result::BLOCKED;
    }
    let (cell, own_now) = if it.action == Action::Harvest {
        (cell - moved, *own + moved)
    } else {
        (cell + moved, *own - moved)
    };
    layer[from] = clamp_field(cell, def.max);
    *own = own_now;
    result::OK
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
    let ((ox, oy), local) = vm::offset_cell(cell, dx, dy);
    if (ox, oy) == (0, 0) {
        Where::Here(local)
    } else {
        Where::Elsewhere(ChunkCoord::new(coord.x + ox, coord.y + oy), local)
    }
}

/// Does `(dx, dy)` from `cell` leave the chunk's 3x3 halo, the reach of
/// every target (RULES §11)?
#[inline]
fn beyond(cell: usize, dx: i8, dy: i8) -> bool {
    let ((ox, oy), _) = vm::offset_cell(cell, dx, dy);
    ox.abs() > 1 || oy.abs() > 1
}

/// Local index of `(dx, dy)` from `cell`, or `None` if it leaves the chunk.
#[inline]
fn local_target(cell: usize, dx: i8, dy: i8) -> Option<usize> {
    let ((ox, oy), local) = vm::offset_cell(cell, dx, dy);
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
        born: stamp(tick),
        last_think: stamp(tick),
        needs,
        mem: [0; MEM_SLOTS],
        state: 0,
        events: 0,
        hurt: 0,
        hurt_dir: 0,
        _pad: 0,
    }
}

/// The mind of a child `spawn`ed at `pos` this tick: its uid (every later
/// key, stagger and dice stream derive from it), a [`newborn`], and the
/// `with` memory. Every run-time birth goes through here. A cell takes one
/// standing and one cover birth a tick, so the two layers hash apart.
fn spawn_mind(
    kinds: &Kinds,
    seed: u64,
    tick: u64,
    pos: Pos,
    kind: u16,
    with: [i32; 2],
) -> ActorMind {
    let stream = match kinds.def(kind).layer {
        0 => STREAM_UID,
        1 => STREAM_UID_COVER,
        l => STREAM_UID_LAYER | u64::from(l) << 16,
    };
    let uid = hash_cell(seed, stream, pos.x, pos.y) ^ splitmix64(tick);
    let mut child = newborn(kinds, kind, uid, tick);
    child.mem[..2].copy_from_slice(&with);
    child
}

/// Change a row's kind in place: consumable needs carry over by name
/// (clamped to the new max), point needs and needs the new kind adds start
/// at max, memory carries by name, state resets, born is now.
fn change_kind(kinds: &Kinds, tick: u64, actors: &mut ActorsMut<'_>, slot: usize, to: u16) -> u8 {
    if usize::from(to) >= kinds.len() {
        return result::REFUSED;
    }
    let layer = actors.pubs[slot].layer;
    if kinds.def(to).layer != layer {
        return result::REFUSED; // a row cannot change layers
    }
    if actors.pubs[slot].heads > kinds.def(to).max_heads() {
        return result::REFUSED; // a group becomes only what holds its heads
    }
    if kinds.def(to).is_big() || kinds.def(actors.pubs[slot].kind).is_big() {
        // Its parts carry its kind, maybe in chunks Apply can't write:
        // a big actor neither becomes nor is become.
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
    m.born = stamp(tick);
    actors.pubs[slot].kind = to;
    let cell = usize::from(actors.pubs[slot].cell);
    actors.cells.layer_mut(layer)[cell] = ActorId::pack(to, slot_u16(slot));
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
    coords: Query<&ChunkCoord>,
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
        let big = match fx.what {
            EffectKind::Move => chunks
                .get(src_e)
                .is_ok_and(|(_, p, ..)| kinds.def(p.rows[slot].kind).is_big()),
            EffectKind::Spawn { kind, .. } => kinds.def(kind).is_big(),
            _ => false,
        };
        if big {
            let Ok(&from) = coords.get(src_e) else {
                continue;
            };
            if let Some(res) = place_big(&kinds, seed, tick, &stage, &mut chunks, src_e, from, &fx)
            {
                set_result(&mut chunks, src_e, slot, res);
            }
            continue;
        }
        let Ok([mut src, mut dst]) = chunks.get_many_mut([src_e, dst_e]) else {
            set_result(&mut chunks, src_e, slot, result::BLOCKED);
            continue;
        };
        let (dst_cells, dst_pubs, dst_minds, dst_scratch, dst_meta) = &mut dst;
        // A group gone since it asked (its own `grow` took its last head):
        // nothing of it moves, splits or joins.
        let mover = matches!(
            fx.what,
            EffectKind::Move | EffectKind::Split { .. } | EffectKind::Join
        );
        if mover && src.1.rows[slot].flags & flags::DEAD != 0 {
            continue;
        }
        if fx.what == EffectKind::Join {
            let row = src.1.rows[slot];
            let res = match dst_cells.layer(row.layer)[cell].row() {
                Some((k, t)) if k == row.kind => {
                    let t = usize::from(t);
                    let sum = u32::from(dst_pubs.rows[t].heads) + u32::from(row.heads);
                    if dst_pubs.rows[t].flags & flags::DEAD != 0 {
                        result::MISSED
                    } else if sum > u32::from(kinds.def(k).max_heads()) {
                        result::REFUSED
                    } else {
                        dst_pubs.rows[t].heads =
                            u16::try_from(sum).expect("at most the kind's max");
                        result::OK
                    }
                }
                _ => result::MISSED,
            };
            if res == result::OK {
                let (src_cells, src_pubs, src_minds, src_scratch, src_meta) = &mut src;
                ActorsMut {
                    pubs: &mut src_pubs.rows,
                    minds: &mut src_minds.rows,
                    cells: src_cells,
                }
                .kill(slot);
                src_scratch.deaths += 1;
                if row.layer == 0 {
                    src_scratch.touch(usize::from(row.cell));
                }
                src_meta.dirty = true;
                dst_meta.dirty = true;
            } else {
                set_result_in(&mut src.2, slot, res);
            }
            continue;
        }
        // The ground's own: a cell taken by nobody, laid or cleared.
        if let EffectKind::Build { material } = fx.what {
            let ok = dst_cells.walkable(cell, &kinds.materials.table)
                && dst_cells.occupant[cell].is_none()
                && dst_scratch.enterable(cell)
                && lay(&kinds, dst_cells, cell, material);
            if ok {
                dst_scratch.touch(cell);
                dst_meta.dirty = true;
            }
            set_result_in(
                &mut src.2,
                slot,
                if ok { result::OK } else { result::BLOCKED },
            );
            continue;
        }
        if fx.what == EffectKind::Dig {
            let ok = dst_cells.feature[cell] != Feature::None;
            if ok {
                dst_cells.feature[cell] = Feature::None;
                dst_scratch.touch(cell);
                dst_meta.dirty = true;
            }
            set_result_in(
                &mut src.2,
                slot,
                if ok { result::OK } else { result::BLOCKED },
            );
            continue;
        }
        let layer = match fx.what {
            EffectKind::Spawn { kind, .. } => kinds.def(kind).layer,
            _ => src.1.rows[slot].layer,
        };
        let walkable = dst_cells.walkable(cell, &kinds.materials.table);
        if let EffectKind::Split { heads } = fx.what
            && heads >= src.1.rows[slot].heads
        {
            set_result_in(&mut src.2, slot, result::REFUSED);
            continue;
        }
        let ok = match layer {
            0 => walkable && dst_cells.occupant[cell].is_none() && dst_scratch.enterable(cell),
            1 => walkable && dst_cells.cover[cell].is_none(),
            l => {
                usize::from(l) < 2 + dst_cells.extra.len()
                    && dst_cells.layer(l)[cell].is_none()
                    && (walkable || kinds.flies(l))
                    && kinds
                        .rest(l)
                        .holds(dst_cells.occupant[cell], &kinds.tag_bits)
            }
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
                let new = to.push_in(cell, row.kind, mind, layer);
                let moved = &mut to.pubs[usize::from(new)];
                moved.signal = row.signal;
                moved.look = row.look;
                moved.heads = row.heads;
                moved.flags = row.flags & flags::WAKE;
                let mut from = ActorsMut {
                    pubs: &mut src_pubs.rows,
                    minds: &mut src_minds.rows,
                    cells: src_cells,
                };
                from.kill(slot);
                src_scratch.deaths += 1;
                if layer == 0 {
                    src_scratch.touch(usize::from(row.cell));
                }
            }
            EffectKind::Spawn { kind, with } => {
                let child = spawn_mind(&kinds, seed, tick, fx.to.cell(cell), kind, with);
                to.push_in(cell, kind, child, layer);
                dst_scratch.count(kind, life::BORN);
                set_result_in(src_minds, slot, result::OK);
            }
            EffectKind::Split { heads } => {
                let kind = src_pubs.rows[slot].kind;
                let child = split_mind(
                    &kinds,
                    seed,
                    tick,
                    fx.to.cell(cell),
                    kind,
                    &src_minds.rows[slot],
                );
                let new = to.push_in(cell, kind, child, layer);
                to.pubs[usize::from(new)].heads = heads;
                src_pubs.rows[slot].heads -= heads;
                set_result_in(src_minds, slot, result::OK);
            }
            EffectKind::Bite { .. }
            | EffectKind::Transfer { .. }
            | EffectKind::Build { .. }
            | EffectKind::Dig
            | EffectKind::Join => unreachable!("handled above"),
        }
        if layer == 0 {
            dst_scratch.touch(cell);
        }
        dst_meta.dirty = true;
        src_meta.dirty = true;
    }
}

/// A big actor's move (its anchor to `fx.cell` of `fx.to`) or birth: every
/// cell of its footprint loaded, walkable unless its layer flies, and
/// empty in its layer or its own, untouched this tick where it stands.
/// Then the old cells are cleared and the new ones written: the anchor
/// holds the row (moved to another chunk if the anchor crossed), the rest
/// parts. `None`: the mover is gone (nothing to say).
#[allow(clippy::too_many_arguments)]
fn place_big(
    kinds: &Kinds,
    seed: u64,
    tick: u64,
    stage: &Stage,
    chunks: &mut ChunkQuery,
    src_e: Entity,
    from: ChunkCoord,
    fx: &Effect,
) -> Option<u8> {
    let slot = usize::from(fx.slot);
    let row = chunks.get(src_e).ok()?.1.rows[slot];
    let mover = fx.what == EffectKind::Move;
    if mover && row.flags & flags::DEAD != 0 {
        return None;
    }
    let kind = match fx.what {
        EffectKind::Spawn { kind, .. } => kind,
        _ => row.kind,
    };
    let def = kinds.def(kind);
    let (l, flies) = (def.layer, kinds.flies(def.layer));
    let (w, h) = (i32::from(def.size.0), i32::from(def.size.1));
    let old = from.cell(usize::from(row.cell));
    let new = fx.to.cell(usize::from(fx.cell));
    let cells_of = |a: Pos| {
        (0..h).flat_map(move |dy| (0..w).map(move |dx| (dx, dy, Pos::new(a.x + dx, a.y + dy))))
    };
    for (_, _, p) in cells_of(new) {
        let (c, i) = p.split();
        let Some((cells, _, _, scratch, _)) = stage.entity(c).and_then(|e| chunks.get(e).ok())
        else {
            return Some(result::BLOCKED);
        };
        if usize::from(l) >= 2 + cells.extra.len()
            || !(flies || cells.walkable(i, &kinds.materials.table))
        {
            return Some(result::BLOCKED);
        }
        let id = cells.layer(l)[i];
        let own = mover
            && match id.part_of() {
                Some((k, ox, oy)) => k == kind && Pos::new(p.x - ox, p.y - oy) == old,
                None => id.row() == Some((kind, fx.slot)) && p == old,
            };
        if !own && (!id.is_none() || (l == 0 && !scratch.enterable(i))) {
            return Some(result::BLOCKED);
        }
    }
    // Clear the old footprint (a move), then write the new one.
    if mover {
        for (dx, dy, p) in cells_of(old).filter(|&(dx, dy, _)| (dx, dy) != (0, 0)) {
            let (c, i) = p.split();
            if let Some(e) = stage.entity(c)
                && let Ok((mut cells, _, _, mut scratch, mut meta)) = chunks.get_mut(e)
                && cells.layer(l)[i] == part_id(kind, dx, dy)
            {
                cells.layer_mut(l)[i] = ActorId::NONE;
                if l == 0 {
                    scratch.touch(i);
                }
                meta.dirty = true;
            }
        }
    }
    let (to, cell) = new.split();
    let dst_e = stage.entity(to).expect("checked loaded above");
    if mover && dst_e == src_e {
        let (mut cells, mut pubs, _, mut scratch, mut meta) = chunks.get_mut(src_e).ok()?;
        cells.layer_mut(l)[usize::from(row.cell)] = ActorId::NONE;
        cells.layer_mut(l)[cell] = ActorId::pack(kind, fx.slot);
        pubs.rows[slot].cell = cell_u16(cell);
        if l == 0 {
            scratch.touch(usize::from(row.cell));
            scratch.touch(cell);
        }
        meta.dirty = true;
    } else {
        let mind = if mover {
            let (_, _, minds, _, _) = chunks.get(src_e).ok()?;
            let mut m = minds.rows[slot];
            m.events = (m.events & !result::MASK) | result::OK;
            m
        } else {
            let EffectKind::Spawn { with, .. } = fx.what else {
                unreachable!("a move or a birth")
            };
            spawn_mind(kinds, seed, tick, new, kind, with)
        };
        if mover {
            let (mut cells, mut pubs, mut minds, mut scratch, mut meta) =
                chunks.get_mut(src_e).ok()?;
            ActorsMut {
                pubs: &mut pubs.rows,
                minds: &mut minds.rows,
                cells: &mut cells,
            }
            .kill(slot);
            scratch.deaths += 1;
            if l == 0 {
                scratch.touch(usize::from(row.cell));
            }
            meta.dirty = true;
        }
        let (mut cells, mut pubs, mut minds, mut scratch, mut meta) = chunks.get_mut(dst_e).ok()?;
        let mut to_rows = ActorsMut {
            pubs: &mut pubs.rows,
            minds: &mut minds.rows,
            cells: &mut cells,
        };
        let n = usize::from(to_rows.push_in(cell, kind, mind, l));
        if mover {
            let moved = &mut to_rows.pubs[n];
            moved.signal = row.signal;
            moved.look = row.look;
            moved.heads = row.heads;
            moved.flags = row.flags & flags::WAKE;
        } else {
            to_rows.pubs[n].heads = def.start_heads();
            scratch.count(kind, life::BORN);
        }
        if l == 0 {
            scratch.touch(cell);
        }
        meta.dirty = true;
    }
    for (dx, dy, p) in cells_of(new).filter(|&(dx, dy, _)| (dx, dy) != (0, 0)) {
        let (c, i) = p.split();
        let e = stage.entity(c).expect("checked loaded above");
        let (mut cells, _, _, mut scratch, mut meta) = chunks.get_mut(e).ok()?;
        cells.layer_mut(l)[i] = part_id(kind, dx, dy);
        if l == 0 {
            scratch.touch(i);
        }
        meta.dirty = true;
    }
    // A mover that crossed into another chunk says OK from its new row.
    Some(if mover && dst_e != src_e {
        result::NONE
    } else {
        result::OK
    })
}

/// The part `(dx, dy)` of an actor of `kind` (both small: in a footprint).
fn part_id(kind: u16, dx: i32, dy: i32) -> ActorId {
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // 0..16
    ActorId::part(kind, dx as u8, dy as u8)
}

/// After Migrate, one thread: the parts of every big actor that died this
/// tick (eaten, starved, its own `die`, or moved to another chunk, whose
/// new parts are already written) are cleared, in whatever chunk they lie.
/// Only a part that still points back at the dead one's cell goes.
pub fn unpart(
    kinds: Res<Kinds>,
    stage: Res<Stage>,
    mut work: Local<Vec<(Pos, u8, u16, (u8, u8))>>,
    mut chunks: Query<(&ChunkCoord, &mut ChunkCells, &ChunkActors, &Scratch)>,
) {
    if !kinds.any_big() {
        return;
    }
    work.clear();
    for &(c, e) in stage.active() {
        let Ok((_, _, pubs, scratch)) = chunks.get(e) else {
            continue;
        };
        if scratch.deaths == 0 {
            continue;
        }
        for r in &pubs.rows {
            let def = kinds.def(r.kind);
            if r.flags & flags::DEAD != 0 && def.is_big() {
                work.push((c.cell(usize::from(r.cell)), r.layer, r.kind, def.size));
            }
        }
    }
    for &(a, l, k, (w, h)) in work.iter() {
        for dy in 0..i32::from(h) {
            for dx in 0..i32::from(w) {
                if (dx, dy) == (0, 0) {
                    continue;
                }
                let (c, i) = Pos::new(a.x + dx, a.y + dy).split();
                if let Some(e) = stage.entity(c)
                    && let Ok((_, mut cells, _, _)) = chunks.get_mut(e)
                    && cells.layer(l)[i] == part_id(k, dx, dy)
                {
                    cells.layer_mut(l)[i] = ActorId::NONE;
                }
            }
        }
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
        for (k, c, n) in s.causes.drain(..) {
            *total.deaths.entry((k, c)).or_default() += u64::from(n);
        }
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

/// The arena's `budget { rows N }` (`docs/RULES.md` §15): no player has
/// more than N rows alive. Each chunk counts its players' rows and the
/// spawns they want this tick, in parallel; then, in coordinate order,
/// each chunk gets what is left of a player's room for its spawns. Apply
/// spends it in key order and refuses the rest (`REFUSED`), so the cap is
/// never passed, on any number of threads.
pub fn ration(
    kinds: Res<Kinds>,
    stage: Res<Stage>,
    mut q: Query<(&ChunkActors, &Intents, &mut Scratch)>,
) {
    let players = &kinds.players;
    let Some(cap) = players.rows else {
        return;
    };
    let n = players.names.len();
    q.par_iter_mut().for_each(|(pubs, intents, mut s)| {
        s.players.clear();
        s.players.resize(n, (0, 0));
        for r in &pubs.rows {
            if let Some(p) = players.of(r.kind) {
                s.players[p].0 += 1;
            }
        }
        for it in intents
            .list
            .iter()
            .filter(|i| matches!(i.action, Action::Spawn | Action::Split))
        {
            if let Some(p) = players.of(it.kind) {
                s.players[p].1 += 1;
            }
        }
    });
    let mut left = vec![cap; n];
    for &(_, e) in stage.active() {
        if let Ok((_, _, s)) = q.get(e) {
            for (l, &(alive, _)) in left.iter_mut().zip(&s.players) {
                *l = l.saturating_sub(alive);
            }
        }
    }
    for &(_, e) in stage.active() {
        let Ok((_, _, mut s)) = q.get_mut(e) else {
            continue;
        };
        let s = &mut *s;
        s.quota.clear();
        for (l, &(_, wants)) in left.iter_mut().zip(&s.players) {
            let take = wants.min(*l);
            *l -= take;
            s.quota.push(take);
        }
    }
}

// ---- Compact ----------------------------------------------------------------------------

pub fn compact(
    kinds: Res<Kinds>,
    mut q: Query<(
        &mut ChunkCells,
        &mut ChunkActors,
        &mut ChunkMinds,
        &mut Intents,
        &mut Scratch,
        &mut Outbox,
    )>,
) {
    let kinds = &*kinds;
    let rests = kinds.any_rest();
    q.par_iter_mut().for_each(
        |(mut cells, mut pubs, mut minds, mut intents, mut scratch, mut outbox)| {
            intents.list.clear();
            outbox.list.clear();
            scratch.reset();
            if rests {
                fall(kinds, &mut cells, &mut pubs, &mut minds, &mut scratch);
            }
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

/// Every row of a layer that rests on a kind (`layer NAME { on KIND }`)
/// with none of it standing beneath at the end of the tick falls, and
/// dies (`Cause::Fell`): its tree was felled, or went off. In slot order;
/// in its own chunk, so in parallel.
fn fall(
    kinds: &Kinds,
    cells: &mut ChunkCells,
    pubs: &mut ChunkActors,
    minds: &mut ChunkMinds,
    scratch: &mut Scratch,
) {
    for slot in 0..pubs.rows.len() {
        let r = pubs.rows[slot];
        if r.layer < 2
            || r.flags & flags::DEAD != 0
            || kinds
                .rest(r.layer)
                .holds(cells.occupant[usize::from(r.cell)], &kinds.tag_bits)
        {
            continue;
        }
        ActorsMut {
            pubs: &mut pubs.rows,
            minds: &mut minds.rows,
            cells,
        }
        .kill(slot);
        scratch.deaths += 1;
        scratch.add(r.kind, life::DIED, u32::from(r.heads));
        scratch
            .causes
            .push((r.kind, Cause::Fell, u32::from(r.heads)));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rules::{SEED, TREE};

    fn halo_one<'a>(cells: &'a ChunkCells, actors: &'a ChunkActors) -> Halo<'a> {
        let mut chunks = [None; 9];
        chunks[4] = Some((cells, actors));
        Halo {
            flies: 0,
            rests: 0,
            chunks,
            tags: &[],
            family_end: &[],
            materials: &crate::stage::MaterialTable::DEFAULT,
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
            layer: 0,
            _pad: 0,
            heads: 1,
            _pad2: 0,
        };
        assert!(due(5, &row, 8)); // (5 + 3) % 8 == 0
        assert!(!due(6, &row, 8));
        assert!(due(13, &row, 8));
        assert!(due(6, &row, 1)); // cadence 1: every tick
        row.flags = flags::WAKE;
        assert!(due(6, &row, 8));
        // Above 2^16 the 16-bit stagger is spread over the whole period:
        // one stagger per even phase, none crowded into its last 65536.
        let slow = 1 << 17;
        row.flags = 0;
        row.stagger = 0xC000;
        assert!(due(32768, &row, slow) && !due(81920, &row, slow));
        for t in [2, 4096, 32768, 65536, 98304, 131070] {
            let n = (0..=u16::MAX)
                .filter(|&s| due(t, &ActorPub { stagger: s, ..row }, slow))
                .count();
            assert_eq!(n, 1, "tick {t}");
        }
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
            kind: 0,
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
    }

    #[test]
    fn a_target_off_the_chunk_is_in_the_neighbour() {
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
        cells.ground[9 * 64 + 11] = crate::stage::Ground::Water;
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

    #[test]
    fn dir_index_numbers_the_unit_steps_from_one() {
        assert_eq!(vm::dir_index(0, -1), 1);
        assert_eq!(vm::dir_index(-1, -1), 8);
        assert_eq!(vm::dir_index(0, 0), 0);
    }
}
