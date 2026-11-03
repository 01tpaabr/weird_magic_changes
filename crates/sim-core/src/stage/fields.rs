//! Field laws in the tick (`docs/WORLD.md` §3): the Simulate phase runs
//! every law due in a chunk, in two systems. [`law_compute`] reads every
//! chunk's fields as they stood before any law ran and writes each due
//! chunk's new layers to its [`LawScratch`]; [`law_commit`] then copies them
//! in. A chunk reads its neighbours' fields in the first pass and writes
//! only its own in the second, so both run in parallel and no order of
//! chunks changes a value.

use std::cell::RefCell;

use bevy_ecs::prelude::*;

use super::{CHUNK_CELLS, ChunkCells, ChunkCoord, ChunkMeta, Stage};
use crate::actors::ChunkActors;
use crate::rules::Kinds;
use crate::rules::law::{self, LawCtx};
use crate::sim::{SimConfig, Tick};
use crate::time::{Clock, daylight};

/// The layers a chunk's laws computed this tick, by field, waiting for
/// [`law_commit`]. Cleared by [`law_compute`]; its capacity stays.
#[derive(Component, Debug, Default)]
pub struct LawScratch {
    pub out: Vec<(u8, [u16; CHUNK_CELLS])>,
}

thread_local! {
    /// A law's columns end to end (`law::COLUMN` values each), per thread,
    /// grown to the longest law and kept.
    static COLS: RefCell<Vec<i32>> = const { RefCell::new(Vec::new()) };
}

/// Run every law due at `tick` in chunk `coord` into `out` (cleared first),
/// from `chunks`: the chunk and its eight neighbours as they stand, in row
/// order from the top-left, and their actors (`is(here, ...)` reads them).
/// The one law runner: the tick and `wmc why` (which must see the fields
/// the think will see) both call it.
pub fn run_due(
    kinds: &Kinds,
    seed: u64,
    tick: u64,
    coord: ChunkCoord,
    chunks: [Option<&ChunkCells>; 9],
    actors: [Option<&ChunkActors>; 9],
    out: &mut Vec<(u8, [u16; CHUNK_CELLS])>,
) {
    out.clear();
    let due =
        |f: &crate::rules::FieldDef| f.law.as_ref().is_some_and(|l| law::law_due(tick, coord, l));
    if !any_due(kinds, tick, coord) {
        return;
    }
    let at = Clock::at(tick);
    let clock = [
        i32::from(at.hour),
        i32::try_from(at.day).unwrap_or(i32::MAX),
        i32::from(daylight(tick)),
    ];
    COLS.with_borrow_mut(|cols| {
        for (f, def) in kinds.fields.iter().enumerate() {
            let Some(l) = def.law.as_ref().filter(|_| due(def)) else {
                continue;
            };
            if l.quiet && still(l, kinds, &chunks) {
                continue; // it would write 0 over 0
            }
            let f = u8::try_from(f).expect("at most MAX_FIELDS fields");
            let ctx = LawCtx {
                seed,
                tick,
                coord,
                field: f,
                clock,
                chunks,
                actors,
                kinds,
            };
            out.push((f, [0; CHUNK_CELLS]));
            let layer = &mut out.last_mut().expect("just pushed").1;
            law::run(l, &ctx, def.max, cols, layer);
        }
    });
}

/// Is any law due in chunk `coord` at `tick`? Most ticks none is (a law
/// runs every N ticks): then nothing around the chunk is looked up.
pub fn any_due(kinds: &Kinds, tick: u64, coord: ChunkCoord) -> bool {
    kinds
        .fields
        .iter()
        .any(|f| f.law.as_ref().is_some_and(|l| law::law_due(tick, coord, l)))
}

/// Is every field law `l` reads 0 all over the chunk, and over its
/// neighbours too if it reads them (where one isn't loaded, the law reads
/// the chunk's own edge, or the field's `outside`)?
fn still(l: &law::Law, kinds: &Kinds, chunks: &[Option<&ChunkCells>; 9]) -> bool {
    let zero = |c: &ChunkCells, f: usize| {
        c.fields
            .get(f)
            .is_none_or(|l| l.iter().fold(0, |a, &v| a | v) == 0)
    };
    let (mut fields, neighbours) = l.reads();
    fields.all(|f| {
        let f = usize::from(f);
        let own = chunks[4].is_none_or(|c| zero(c, f));
        own && (!neighbours
            || chunks.iter().all(|c| match c {
                Some(c) => zero(c, f),
                None => kinds.fields[f].outside.is_none_or(|v| v == 0),
            }))
    })
}

/// The chunk at `c` and its eight neighbours, by `get`, in row order from
/// the top-left.
pub fn around<T>(c: ChunkCoord, get: impl Fn(ChunkCoord) -> Option<T>) -> [Option<T>; 9] {
    std::array::from_fn(|k| {
        let k = i32::try_from(k).expect("0..9");
        get(ChunkCoord::new(c.x + k % 3 - 1, c.y + k / 3 - 1))
    })
}

/// Run every law due in each chunk, from the fields as they stand, into
/// the chunk's [`LawScratch`].
pub fn law_compute(
    tick: Res<Tick>,
    cfg: Res<SimConfig>,
    kinds: Res<Kinds>,
    stage: Res<Stage>,
    cells: Query<&ChunkCells>,
    rows: Query<&ChunkActors>,
    mut own: Query<(&ChunkCoord, &mut LawScratch)>,
) {
    let (tick, seed, kinds, stage) = (tick.0, cfg.seed, &*kinds, &*stage);
    let (cells, rows) = (&cells, &rows);
    if !kinds.fields.iter().any(|f| f.law.is_some()) {
        // No law, but what one computed before a reload to rules without
        // any must not be committed again, every tick.
        for (_, mut scratch) in &mut own {
            if !scratch.out.is_empty() {
                scratch.out.clear();
            }
        }
        return;
    }
    own.par_iter_mut().for_each(|(coord, mut scratch)| {
        if !any_due(kinds, tick, *coord) {
            scratch.out.clear();
            return;
        }
        let chunks = around(*coord, |c| cells.get(stage.entity(c)?).ok());
        let actors = around(*coord, |c| rows.get(stage.entity(c)?).ok());
        run_due(kinds, seed, tick, *coord, chunks, actors, &mut scratch.out);
    });
}

/// Copy what [`law_compute`] found into each chunk's fields; a chunk whose
/// fields changed is dirty.
pub fn law_commit(mut q: Query<(&mut ChunkCells, &LawScratch, &mut ChunkMeta)>) {
    q.par_iter_mut().for_each(|(mut cells, scratch, mut meta)| {
        for (f, layer) in &scratch.out {
            let Some(own) = cells.fields.get(usize::from(*f)) else {
                continue;
            };
            if own != layer {
                cells.fields[usize::from(*f)] = *layer;
                meta.dirty = true;
            }
        }
    });
}
