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

/// Run every law due in each chunk, from the fields as they stand, into
/// the chunk's [`LawScratch`].
pub fn law_compute(
    tick: Res<Tick>,
    cfg: Res<SimConfig>,
    kinds: Res<Kinds>,
    stage: Res<Stage>,
    cells: Query<&ChunkCells>,
    mut own: Query<(Entity, &ChunkCoord, &mut LawScratch)>,
) {
    let (tick, seed, kinds, stage, cells) = (tick.0, cfg.seed, &*kinds, &*stage, &cells);
    if !kinds.fields.iter().any(|f| f.law.is_some()) {
        return;
    }
    let at = Clock::at(tick);
    let clock = [
        i32::from(at.hour),
        i32::try_from(at.day).unwrap_or(i32::MAX),
        i32::from(daylight(tick)),
    ];
    own.par_iter_mut().for_each(|(e, coord, mut scratch)| {
        scratch.out.clear();
        let due = |f: &crate::rules::FieldDef| {
            f.law
                .as_ref()
                .is_some_and(|l| law::law_due(tick, *coord, l.every))
        };
        if !kinds.fields.iter().any(due) {
            return;
        }
        let Ok(me) = cells.get(e) else {
            return;
        };
        let chunks: [Option<&ChunkCells>; 9] = std::array::from_fn(|k| {
            if k == 4 {
                return Some(me);
            }
            let k = i32::try_from(k).expect("0..9");
            let (ox, oy) = (k % 3 - 1, k / 3 - 1);
            let e = stage.entity(ChunkCoord::new(coord.x + ox, coord.y + oy))?;
            cells.get(e).ok()
        });
        COLS.with_borrow_mut(|cols| {
            for (f, def) in kinds.fields.iter().enumerate() {
                let Some(l) = def.law.as_ref().filter(|_| due(def)) else {
                    continue;
                };
                let f = u8::try_from(f).expect("at most MAX_FIELDS fields");
                let ctx = LawCtx {
                    seed,
                    tick,
                    coord: *coord,
                    field: f,
                    clock,
                    chunks,
                    defs: &kinds.fields,
                };
                scratch.out.push((f, [0; CHUNK_CELLS]));
                let out = &mut scratch.out.last_mut().expect("just pushed").1;
                law::run(l, &ctx, def.max, cols, out);
            }
        });
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
