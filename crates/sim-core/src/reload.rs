//! Moving a world between rule sets by name: hot reload (`r` in `wmc
//! play`, [`reload_rules`]) and opening a save with rules that number
//! things differently (`sim::open`, [`PendingRemap`]).
//!
//! Rows refer to kinds, need slots, mem slots, states and scent channels by
//! index; another rule set may number them differently. A [`Plan`] maps
//! everything by name: a row keeps its kind if a kind of that name still
//! exists (rows of a kind that is gone are dropped), each need and mem
//! carries over by name (clamped to the new max; new needs start full, new
//! mems at 0), the state by its name (else the first), each scent channel
//! by its name (else empty). A kind cannot move between standing and ground
//! cover (refused: restart).
//!
//! Saved chunks are rewritten on disk, every file, before the world file
//! gets the new kind table, so a save directory never mixes two numberings.
//! A crash in the middle of that rewrite leaves it mixed; it is not
//! journalled.
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
use crate::stage::{ActorId, ChunkCells, ChunkCoord, ChunkMeta, SCENT_CHANNELS, Stage};
use crate::store::{SavedKind, Store};

/// What a reload changed.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Reload {
    /// Kinds the new rules add, and those they drop (with the rows dropped).
    pub added: Vec<String>,
    pub removed: Vec<(String, usize)>,
    /// Saved chunk files rewritten.
    pub rewritten: usize,
    /// The new rules hash.
    pub hash: u64,
}

/// Where one old kind's rows go.
#[derive(Debug, Clone, PartialEq, Eq)]
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

/// How rows and scent written under one rule set map onto another.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plan {
    /// Per old kind; `None` = the kind is gone.
    kinds: Vec<Option<Target>>,
    /// Per new channel: the old channel of that name.
    scents: [Option<usize>; SCENT_CHANNELS],
}

/// A save opened with rules that number things differently: the plan every
/// chunk read from its store goes through, until the first write to the
/// store migrates the whole directory (`sim::settle`).
#[derive(Resource, Debug, Clone, Default)]
pub struct PendingRemap(pub Option<Plan>);

impl Plan {
    /// From the rules a world was written with (`old`, as a save keeps
    /// them) to `new`. A kind that moved between standing and ground cover
    /// is an error.
    pub fn between(old: &[SavedKind], old_scents: &[String], new: &Kinds) -> Result<Plan, String> {
        let mut kinds = Vec::with_capacity(old.len());
        for od in old {
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
                t.needs[i] = od.needs.iter().position(|o| *o == n.name);
                t.maxes[i] = n.max;
            }
            for (i, m) in nd.mems.iter().enumerate() {
                t.mems[i] = od.mems.iter().position(|o| o == m);
            }
            let ns = new
                .debug
                .states
                .get(usize::from(nd.id))
                .cloned()
                .unwrap_or_default();
            t.states = od
                .states
                .iter()
                .map(|s| ns.iter().position(|n| n == s).unwrap_or(0) as u8)
                .collect();
            kinds.push(Some(t));
        }
        let mut scents = [None; SCENT_CHANNELS];
        for (j, name) in new.scents.iter().enumerate() {
            scents[j] = old_scents.iter().position(|o| o == name);
        }
        Ok(Plan { kinds, scents })
    }

    /// How many kinds the old table has: saved rows are checked against it
    /// before they are remapped.
    pub fn old_kinds(&self) -> usize {
        self.kinds.len()
    }

    /// Remap one chunk's rows and scent. Returns whether anything changed;
    /// counts dropped rows per old kind.
    pub fn apply(
        &self,
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
            match self
                .kinds
                .get(usize::from(row.kind))
                .and_then(Option::as_ref)
            {
                None => {
                    actors.kill(slot);
                    if let Some(n) = dropped.get_mut(usize::from(row.kind)) {
                        *n += 1;
                    }
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
            let next = self.scents[j].map_or([0; crate::stage::CHUNK_CELLS], |i| old[i]);
            changed |= *ch != next;
            *ch = next;
        }
        changed
    }
}

/// Rewrite every chunk file in `store` through `plan`, in coordinate order.
/// Dropped rows are counted per old kind, except in the chunks `loaded`
/// (their rows in memory are the ones that count). Returns the files
/// rewritten.
pub fn rewrite_saved(
    store: &Store,
    plan: &Plan,
    loaded: &[ChunkCoord],
    dropped: &mut [usize],
) -> Result<usize, String> {
    let mut scratch = vec![0usize; plan.old_kinds()];
    let mut rewritten = 0;
    for c in store
        .saved_chunks()
        .map_err(|e| format!("listing saved chunks: {e}"))?
    {
        let Some(mut saved) = store
            .read_chunk(c)
            .map_err(|e| format!("reading saved chunk {c:?}: {e}"))?
        else {
            continue;
        };
        let d = &mut saved.data;
        d.validate(plan.old_kinds())
            .map_err(|e| format!("saved chunk {c:?}: {e}"))?;
        let counts = if loaded.contains(&c) {
            &mut scratch[..]
        } else {
            &mut *dropped
        };
        plan.apply(&mut d.cells, &mut d.actors.rows, &mut d.minds.rows, counts);
        store
            .write_chunk(c, &d.cells, &d.actors, &d.minds, saved.last_ticked)
            .map_err(|e| format!("rewriting saved chunk {c:?}: {e}"))?;
        rewritten += 1;
    }
    Ok(rewritten)
}

/// Swap in `new` rules (see the module doc). With a `store`, every saved
/// chunk is rewritten and the world file updated. On an error nothing in
/// the world has changed yet, except for a failed disk rewrite part-way
/// (reported; the chunks already rewritten are consistent with the new
/// rules, which are then not installed).
pub fn reload_rules(
    world: &mut World,
    store: Option<&Store>,
    new: Kinds,
) -> Result<Reload, String> {
    // A save opened under other rules moves to them first: every file in
    // one numbering before the next one starts.
    if let Some(store) = store {
        crate::sim::settle(world, store).map_err(|e| format!("migrating the save: {e}"))?;
    }
    let old = world.resource::<Kinds>().clone();
    let plan = Plan::between(&SavedKind::table(&old), &old.scents, &new)?;
    let (starts, placement) = {
        let c = world.resource::<SimConfig>();
        let starts = present(&c.starts, &new);
        let placement = Placement::resolve(&starts, &new, c.seed, &c.params)?;
        (starts, placement)
    };
    let mut dropped = vec![0usize; old.len()];
    let loaded: Vec<ChunkCoord> = world.resource::<Stage>().loaded_coords().collect();
    let rewritten = match store {
        Some(store) => rewrite_saved(store, &plan, &loaded, &mut dropped)?,
        None => 0,
    };
    for (mut cells, mut pubs, mut minds, mut meta) in world
        .query::<(
            &mut ChunkCells,
            &mut ChunkActors,
            &mut ChunkMinds,
            &mut ChunkMeta,
        )>()
        .iter_mut(world)
    {
        if plan.apply(&mut cells, &mut pubs.rows, &mut minds.rows, &mut dropped) {
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
