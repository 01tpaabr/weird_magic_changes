//! Hot reload: swap the rule set of a running world (`r` in `wmc play`).
//!
//! Rows refer to kinds, need slots, mem slots, states and scent channels by
//! index; a new rule set may number them differently. [`reload_rules`] maps
//! everything by name: a row keeps its kind if a kind of that name still
//! exists (rows of a kind that is gone are dropped), each need and mem
//! carries over by name (clamped to the new max; new needs start full, new
//! mems at 0), the state by its name (else the first), each scent channel
//! by its name (else empty). Loaded chunks are remapped in memory; saved
//! chunks that are not loaded are rewritten on disk, and the world file gets
//! the new kind list, so the save stays readable by this rule set. The
//! scenario's starts are resolved again (kind ids may have moved); those of
//! a kind that is gone are dropped. A kind cannot move between standing and
//! ground cover (refused: restart).
//!
//! Reload is a dev tool. It is an input the replay log does not record: a
//! world that was reloaded is not reproducible from its seed alone.

use bevy_ecs::prelude::*;

use crate::actors::{
    ActorMind, ActorPub, ActorsMut, ChunkActors, ChunkMinds, MEM_SLOTS, NEED_SLOTS, Tally, flags,
};
use crate::rules::Kinds;
use crate::scenario::{Placement, present};
use crate::sim::SimConfig;
use crate::stage::{ActorId, ChunkCells, ChunkMeta, SCENT_CHANNELS, Stage};
use crate::store::Store;

/// What a reload changed.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Reload {
    /// Kinds the new rules add, and those they drop (with the rows dropped).
    pub added: Vec<String>,
    pub removed: Vec<(String, usize)>,
    /// Saved chunks rewritten on disk.
    pub rewritten: usize,
    /// The new rules hash.
    pub hash: u64,
}

/// Where one old kind's rows go.
#[derive(Debug, Clone)]
struct Target {
    kind: u16,
    /// For each new need slot: the old slot of that name.
    needs: [Option<usize>; NEED_SLOTS],
    maxes: [i32; NEED_SLOTS],
    need_count: usize,
    mems: [Option<usize>; MEM_SLOTS],
    /// Old state index -> new.
    states: Vec<u8>,
}

#[derive(Debug, Clone)]
struct Plan {
    /// Per old kind; `None` = the kind is gone.
    kinds: Vec<Option<Target>>,
    /// Per new channel: the old channel of that name.
    scents: [Option<usize>; SCENT_CHANNELS],
}

fn plan(old: &Kinds, new: &Kinds) -> Result<Plan, String> {
    let mut kinds = Vec::with_capacity(old.len());
    for od in &old.defs {
        let Some(nd) = new.by_name(&od.name) else {
            kinds.push(None);
            continue;
        };
        if nd.cover != od.cover {
            return Err(format!(
                "kind `{}` moved between standing and ground cover: restart to change that",
                od.name
            ));
        }
        let mut t = Target {
            kind: nd.id,
            needs: [None; NEED_SLOTS],
            maxes: [0; NEED_SLOTS],
            need_count: nd.needs.len(),
            mems: [None; MEM_SLOTS],
            states: Vec::new(),
        };
        for (i, n) in nd.needs.iter().enumerate() {
            t.needs[i] = od.need_named(&n.name);
            t.maxes[i] = n.max;
        }
        for (i, m) in nd.mems.iter().enumerate() {
            t.mems[i] = od.mems.iter().position(|o| o == m);
        }
        let names = |k: &Kinds, id: u16| k.debug.states.get(usize::from(id)).cloned();
        let (os, ns) = (
            names(old, od.id).unwrap_or_default(),
            names(new, nd.id).unwrap_or_default(),
        );
        t.states = os
            .iter()
            .map(|s| ns.iter().position(|n| n == s).unwrap_or(0) as u8)
            .collect();
        kinds.push(Some(t));
    }
    let mut scents = [None; SCENT_CHANNELS];
    for (j, name) in new.scents.iter().enumerate() {
        scents[j] = old.scents.iter().position(|o| o == name);
    }
    Ok(Plan { kinds, scents })
}

/// Remap one chunk's rows and scent. Returns whether anything changed;
/// counts dropped rows per old kind.
fn remap(
    plan: &Plan,
    cells: &mut ChunkCells,
    pubs: &mut Vec<ActorPub>,
    minds: &mut Vec<ActorMind>,
    dropped: &mut [usize],
) -> bool {
    let mut changed = !pubs.is_empty();
    let mut actors = ActorsMut { pubs, minds, cells };
    let mut dead = false;
    for slot in 0..actors.pubs.len() {
        let row = actors.pubs[slot];
        match plan
            .kinds
            .get(usize::from(row.kind))
            .and_then(Option::as_ref)
        {
            None => {
                actors.kill(slot);
                dropped[usize::from(row.kind)] += 1;
                dead = true;
            }
            Some(t) => {
                actors.pubs[slot].kind = t.kind;
                let cover = row.flags & flags::COVER != 0;
                actors.layer(cover)[usize::from(row.cell)] = ActorId::pack(t.kind, slot as u16);
                let old = actors.minds[slot];
                let m = &mut actors.minds[slot];
                for i in 0..NEED_SLOTS {
                    m.needs[i] = match t.needs[i] {
                        Some(src) => old.needs[src].clamp(0, t.maxes[i]),
                        None if i < t.need_count => t.maxes[i],
                        None => 0,
                    };
                }
                for i in 0..MEM_SLOTS {
                    m.mem[i] = t.mems[i].map_or(0, |src| old.mem[src]);
                }
                m.state = t.states.get(usize::from(old.state)).copied().unwrap_or(0);
            }
        }
    }
    if dead {
        actors.compact();
    }
    let old = cells.scent;
    for (j, ch) in cells.scent.iter_mut().enumerate() {
        let next = plan.scents[j].map_or([0; crate::stage::CHUNK_CELLS], |i| old[i]);
        changed |= *ch != next;
        *ch = next;
    }
    changed
}

/// Swap in `new` rules (see the module doc). With a `store`, saved chunks
/// that are not loaded are rewritten and the world file updated. On an
/// error nothing in the world has changed yet, except for a failed disk
/// rewrite part-way (reported; the chunks already rewritten are consistent
/// with the new rules, which are then not installed).
pub fn reload_rules(
    world: &mut World,
    store: Option<&Store>,
    new: Kinds,
) -> Result<Reload, String> {
    let old = world.resource::<Kinds>().clone();
    let plan = plan(&old, &new)?;
    let (starts, placement) = {
        let c = world.resource::<SimConfig>();
        let starts = present(&c.starts, &new);
        let placement = Placement::resolve(&starts, &new, c.seed, &c.params)?;
        (starts, placement)
    };
    let mut dropped = vec![0usize; old.len()];
    let loaded: Vec<crate::stage::ChunkCoord> = world.resource::<Stage>().loaded_coords().collect();
    let mut rewritten = 0;
    if let Some(store) = store {
        for c in store
            .saved_chunks()
            .map_err(|e| format!("listing saved chunks: {e}"))?
        {
            if loaded.contains(&c) {
                continue;
            }
            let Some(mut saved) = store
                .read_chunk(c)
                .map_err(|e| format!("reading saved chunk {c:?}: {e}"))?
            else {
                continue;
            };
            let d = &mut saved.data;
            remap(
                &plan,
                &mut d.cells,
                &mut d.actors.rows,
                &mut d.minds.rows,
                &mut dropped,
            );
            store
                .write_chunk(c, &d.cells, &d.actors, &d.minds, saved.last_ticked)
                .map_err(|e| format!("rewriting saved chunk {c:?}: {e}"))?;
            rewritten += 1;
        }
    }
    for (mut cells, mut pubs, mut minds, mut meta) in world
        .query::<(
            &mut ChunkCells,
            &mut ChunkActors,
            &mut ChunkMinds,
            &mut ChunkMeta,
        )>()
        .iter_mut(world)
    {
        if remap(
            &plan,
            &mut cells,
            &mut pubs.rows,
            &mut minds.rows,
            &mut dropped,
        ) {
            meta.dirty = true;
        }
    }
    let report = Reload {
        added: new
            .names()
            .filter(|n| old.by_name(n).is_none())
            .map(str::to_string)
            .collect(),
        removed: old
            .defs
            .iter()
            .filter(|d| new.by_name(&d.name).is_none())
            .map(|d| (d.name.clone(), dropped[usize::from(d.id)]))
            .collect(),
        rewritten,
        hash: new.hash,
    };
    world.insert_resource(new);
    world.insert_resource(Tally::default());
    let mut c = world.resource_mut::<SimConfig>();
    (c.starts, c.placement) = (starts, placement);
    if let Some(store) = store {
        store
            .write_meta(&crate::sim::meta(world))
            .map_err(|e| format!("writing the world file: {e}"))?;
    }
    Ok(report)
}
