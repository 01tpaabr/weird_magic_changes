//! Moving a world between rule sets by name: hot reload (`r` in `wmc
//! play`, [`reload_rules`]) and opening a save with rules that number
//! things differently (`sim::open`, [`PendingRemap`]).
//!
//! Rows refer to kinds, need slots, mem slots, states and fields by
//! index; another rule set may number them differently. A [`Plan`] maps
//! everything by name: a row keeps its kind if a kind of that name still
//! exists (rows of a kind that is gone are dropped), each need and mem
//! carries over by name (clamped to the new max; new needs start full, new
//! mems at 0), the state by its name (else the first), each field by its
//! name (clamped to its max; a new one starts at its `init`), a group's
//! heads (clamped to the new kind's max: 1 if it is no longer a group), each
//! declared layer by its name. A kind cannot move between layers (standing,
//! ground cover, a declared one, by name; refused: restart). The world's starts are all kept: one whose
//! kind is gone places nobody (a share keeps its interval, so the others
//! keep their cells) and starts again when the kind comes back.
//!
//! Saved chunks are rewritten on disk, every file, before the world file
//! gets the new kind table, so a save directory never mixes two numberings.
//! Every file is read and checked before the first is written: a bad one
//! refuses the move with nothing written. An I/O error or a crash in the
//! middle of the writes leaves it mixed; it is not journalled.
//!
//! Reload is a dev tool. It is an input the replay log does not record: a
//! world that was reloaded is not reproducible from its seed alone.

use bevy_ecs::prelude::*;

use crate::actors::{
    ActorMind, ActorPub, ActorsMut, ChunkActors, ChunkMinds, MEM_SLOTS, NEED_SLOTS, Tally, slot_u16,
};
use crate::rules::{FieldDef, Kinds};
use crate::scenario::{Placement, absent, present};
use crate::sim::SimConfig;
use crate::stage::worldgen::field_layer;
use crate::stage::{ActorId, ChunkCells, ChunkCoord, ChunkMeta, Stage};
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
    /// Why the world file was not written, if it was not. The new rules are
    /// installed all the same; the next save writes it.
    pub world_file: Option<String>,
    /// The starts naming a kind the new rules lack, as written: kept in
    /// the save, they place nobody until the kind is back.
    pub skipped_starts: Vec<String>,
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
    /// The most heads a row of the new kind holds (1: not a group).
    max_heads: u16,
    /// It covers more than one cell: its parts carry its kind.
    big: bool,
}

/// How rows and fields written under one rule set map onto another.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plan {
    /// Per old kind; `None` = the kind is gone.
    kinds: Vec<Option<Target>>,
    /// Per new field: the old layer of that name (clamped to the new max),
    /// else none and the field starts at its `init`.
    fields: Vec<(Option<usize>, FieldDef)>,
    /// Field layers a chunk written under the old rules holds.
    old_fields: usize,
    /// Per old ground, the new one of its name (else the first); per old
    /// feature (0 none), the new one (else none).
    grounds: Vec<u8>,
    features: Vec<u8>,
    /// Per new declared layer, the old one of its name (else it starts
    /// empty).
    layers: Vec<Option<usize>>,
    /// Declared layers a chunk written under the old rules holds.
    old_layers: usize,
    /// The world's seed: a new field's `init` noise.
    seed: u64,
}

/// A save opened with rules that number things differently: the plan every
/// chunk read from its store goes through, until the first write to the
/// store migrates the whole directory (`sim::settle`).
#[derive(Resource, Debug, Clone, Default)]
pub struct PendingRemap(pub Option<Plan>);

impl Plan {
    /// From the rules a world was written with (`old`, as a save keeps
    /// them) to `new`. A kind that moved between layers is an error.
    pub fn between(
        old: &[SavedKind],
        old_fields: &[String],
        (old_grounds, old_features): (&[String], &[String]),
        old_layers: &[String],
        new: &Kinds,
        seed: u64,
    ) -> Result<Plan, String> {
        let old_name = |l: u8| match l {
            0 => "standing",
            1 => "ground cover",
            l => old_layers
                .get(usize::from(l) - 2)
                .map_or("?", String::as_str),
        };
        let new_name = |l: u8| match l {
            0 => "standing",
            1 => "ground cover",
            l => new
                .layers
                .get(usize::from(l) - 2)
                .map_or("?", |d| d.name.as_str()),
        };
        let mut kinds = Vec::with_capacity(old.len());
        for od in old {
            let Some(nd) = new.by_name(&od.name) else {
                kinds.push(None);
                continue;
            };
            if od.size != nd.size {
                return Err(format!(
                    "kind `{}` changed size from {} x {} to {} x {}: restart to change that",
                    od.name, od.size.0, od.size.1, nd.size.0, nd.size.1
                ));
            }
            if old_name(od.layer) != new_name(nd.layer) {
                return Err(format!(
                    "kind `{}` moved from {} to {}: restart to change that",
                    od.name,
                    old_name(od.layer),
                    new_name(nd.layer)
                ));
            }
            let mut t = Target {
                kind: nd.id,
                needs: [None; NEED_SLOTS],
                maxes: [0; NEED_SLOTS],
                need_count: nd.needs.len(),
                mems: [None; MEM_SLOTS],
                states: Vec::new(),
                max_heads: nd.max_heads(),
                big: nd.is_big(),
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
                .map(|s| {
                    let i = ns.iter().position(|n| n == s).unwrap_or(0);
                    u8::try_from(i).expect("at most MAX_STATES states")
                })
                .collect();
            kinds.push(Some(t));
        }
        let fields = new
            .fields
            .iter()
            .map(|f| (old_fields.iter().position(|o| *o == f.name), f.clone()))
            .collect();
        Ok(Plan {
            kinds,
            fields,
            old_fields: old_fields.len(),
            grounds: old_grounds
                .iter()
                .map(|g| new.materials.ground(g).map_or(0, |g| g.0))
                .collect(),
            features: std::iter::once(0)
                .chain(
                    old_features
                        .iter()
                        .map(|f| new.materials.feature(f).map_or(0, |f| f.0)),
                )
                .collect(),
            layers: new
                .layers
                .iter()
                .map(|l| old_layers.iter().position(|o| *o == l.name))
                .collect(),
            old_layers: old_layers.len(),
            seed,
        })
    }

    /// How many kinds the old table has: saved rows are checked against it
    /// before they are remapped.
    pub fn old_kinds(&self) -> usize {
        self.kinds.len()
    }

    /// The old rules' ground and feature counts: saved chunks index them.
    pub fn old_materials(&self) -> (usize, usize) {
        (self.grounds.len(), self.features.len() - 1)
    }

    /// How many field layers the old rules had: saved chunks hold that many.
    pub fn old_fields(&self) -> usize {
        self.old_fields
    }

    /// How many declared layers the old rules had: saved chunks hold that
    /// many.
    pub fn old_layers(&self) -> usize {
        self.old_layers
    }

    /// Remap one chunk's rows and fields. Returns whether anything
    /// changed; counts dropped rows per old kind.
    pub fn apply(
        &self,
        coord: ChunkCoord,
        cells: &mut ChunkCells,
        pubs: &mut Vec<ActorPub>,
        minds: &mut Vec<ActorMind>,
        dropped: &mut [usize],
    ) -> bool {
        let changed = !pubs.is_empty();
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
                    actors.cells.layer_mut(row.layer)[usize::from(row.cell)] =
                        ActorId::pack(t.kind, slot_u16(slot));
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
                    actors.pubs[slot].heads = row.heads.clamp(1, t.max_heads);
                }
            }
        }
        if dead {
            actors.compact();
        }
        // Parts of big actors carry their kind: renumbered as rows are, or
        // gone with their kind.
        if self.kinds.iter().flatten().any(|t| t.big) {
            for l in 0..2 + actors.cells.extra.len() {
                let l = u8::try_from(l).expect("a few layers");
                for id in actors.cells.layer_mut(l).iter_mut() {
                    if let Some((k, dx, dy)) = id.part_of() {
                        *id = match self.kinds.get(usize::from(k)).and_then(Option::as_ref) {
                            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                            // 0..16
                            Some(t) => ActorId::part(t.kind, dx as u8, dy as u8),
                            None => ActorId::NONE,
                        };
                    }
                }
            }
        }
        // Declared layers by name: the rows left are in layers the new
        // rules declare (`between` checked their kinds).
        let identity = self.layers.len() == self.old_layers
            && self.layers.iter().enumerate().all(|(i, &o)| o == Some(i));
        if !identity {
            let old = std::mem::take(&mut actors.cells.extra);
            let to = |o: usize| self.layers.iter().position(|&n| n == Some(o));
            for row in actors.pubs.iter_mut().filter(|r| r.layer >= 2) {
                let n = to(usize::from(row.layer) - 2).expect("a kept kind's layer is declared");
                row.layer = u8::try_from(n + 2).expect("at most MAX_LAYERS layers");
            }
            actors.cells.extra = self
                .layers
                .iter()
                .map(|o| o.map_or([ActorId::NONE; crate::stage::CHUNK_CELLS], |i| old[i]))
                .collect();
        }
        let same = |m: &[u8]| m.iter().enumerate().all(|(i, &v)| usize::from(v) == i);
        let mut terrain = false;
        if !same(&self.grounds) {
            for g in &mut cells.ground {
                g.0 = self.grounds.get(usize::from(g.0)).copied().unwrap_or(0);
            }
            terrain = true;
        }
        if !same(&self.features) {
            for f in &mut cells.feature {
                f.0 = self.features.get(usize::from(f.0)).copied().unwrap_or(0);
            }
            terrain = true;
        }
        let old = std::mem::take(&mut cells.fields);
        for (src, def) in &self.fields {
            let layer = match src.and_then(|i| old.get(i)) {
                Some(l) => l.map(|v| v.min(def.max)),
                None => field_layer(self.seed, def, coord),
            };
            cells.fields.push(layer);
        }
        changed | terrain | (cells.fields != old)
    }
}

/// The rules' ground names, by index: what a save keeps of them.
pub fn ground_names(kinds: &Kinds) -> Vec<String> {
    kinds
        .materials
        .grounds
        .iter()
        .map(|g| g.name.clone())
        .collect()
}

/// The rules' feature names, from feature 1.
pub fn feature_names(kinds: &Kinds) -> Vec<String> {
    kinds
        .materials
        .features
        .iter()
        .map(|f| f.name.clone())
        .collect()
}

/// The rules' declared layer names, layer 2 first: what a save keeps of
/// them.
pub fn layer_names(kinds: &Kinds) -> Vec<String> {
    kinds.layers.iter().map(|l| l.name.clone()).collect()
}

/// The rules' field names, in layer order: what a save keeps of them.
pub fn field_names(kinds: &Kinds) -> Vec<String> {
    kinds.fields.iter().map(|f| f.name.clone()).collect()
}

/// Rewrite every chunk file in `store` through `plan`, in coordinate order.
/// Every file is read and checked first, so a bad one refuses the whole
/// rewrite with nothing written; then each is read again and written.
/// Dropped rows are counted per old kind, except in the chunks `loaded`
/// (their rows in memory are the ones that count). Returns the files
/// rewritten.
pub fn rewrite_saved(
    store: &Store,
    plan: &Plan,
    loaded: &[ChunkCoord],
    dropped: &mut [usize],
) -> Result<usize, String> {
    let coords = store
        .saved_chunks()
        .map_err(|e| format!("listing saved chunks: {e}"))?;
    let read = |c: ChunkCoord| {
        let saved = store.read_chunk(c).map_err(|e| e.to_string())?;
        if let Some(s) = &saved {
            s.data
                .validate(plan.old_kinds())
                .map_err(|e| format!("saved chunk {c:?}: {e}"))?;
        }
        Ok::<_, String>(saved)
    };
    for &c in &coords {
        read(c)?;
    }
    let mut scratch = vec![0usize; plan.old_kinds()];
    let mut rewritten = 0;
    for c in coords {
        let Some(mut saved) = read(c)? else {
            continue;
        };
        let d = &mut saved.data;
        let counts = if loaded.contains(&c) {
            &mut scratch[..]
        } else {
            &mut *dropped
        };
        plan.apply(
            c,
            &mut d.cells,
            &mut d.actors.rows,
            &mut d.minds.rows,
            counts,
        );
        store
            .write_chunk(c, &d.cells, &d.actors, &d.minds, saved.last_ticked)
            .map_err(|e| format!("rewriting saved chunk {c:?}: {e}"))?;
        rewritten += 1;
    }
    Ok(rewritten)
}

/// Swap in `new` rules (see the module doc). With a `store`, every saved
/// chunk is rewritten and the world file updated. On an error nothing in
/// the world or the store has changed yet (a bad chunk file refuses the
/// rewrite before any is written), except for a chunk write that fails
/// part-way (reported; the chunks already rewritten are consistent with the
/// new rules, which are then not installed). The world file is written last,
/// after the new rules are installed: if that fails, the reload still
/// happened and [`Reload::world_file`] says why (the next save writes it).
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
    let seed = world.resource::<SimConfig>().seed;
    let plan = Plan::between(
        &SavedKind::table(&old),
        &field_names(&old),
        (&ground_names(&old), &feature_names(&old)),
        &layer_names(&old),
        &new,
        seed,
    )?;
    let (starts, placement) = {
        let c = world.resource::<SimConfig>();
        let starts = present(&c.starts, &new);
        let placement = Placement::resolve_saved(&starts, &new, &c.terrain())?;
        (starts, placement)
    };
    let mut dropped = vec![0usize; old.len()];
    let loaded: Vec<ChunkCoord> = world.resource::<Stage>().loaded_coords().collect();
    let rewritten = match store {
        Some(store) => rewrite_saved(store, &plan, &loaded, &mut dropped)?,
        None => 0,
    };
    for (&coord, mut cells, mut pubs, mut minds, mut meta) in world
        .query::<(
            &ChunkCoord,
            &mut ChunkCells,
            &mut ChunkActors,
            &mut ChunkMinds,
            &mut ChunkMeta,
        )>()
        .iter_mut(world)
    {
        if plan.apply(
            coord,
            &mut cells,
            &mut pubs.rows,
            &mut minds.rows,
            &mut dropped,
        ) {
            meta.dirty = true;
        }
    }
    let mut report = Reload {
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
        world_file: None,
        skipped_starts: absent(&starts, &new),
    };
    world.insert_resource(new);
    world.insert_resource(Tally::default());
    let mut c = world.resource_mut::<SimConfig>();
    (c.starts, c.placement) = (starts, placement);
    if let Some(store) = store {
        report.world_file = store
            .write_meta(&crate::sim::meta(world))
            .err()
            .map(|e| e.to_string());
    }
    Ok(report)
}
