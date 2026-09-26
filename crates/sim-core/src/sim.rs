//! The whole simulation: its resources, its tick schedule, creation,
//! streaming, save/load and checksum, all over a `bevy_ecs::World`.
//!
//! A tick is [`SimTick`], a schedule whose systems are grouped into
//! [`Phase`] sets that run in a fixed order. Inside a phase, systems have
//! disjoint data access (Bevy refuses to build the schedule otherwise:
//! ambiguity detection is set to *error*) and iterate chunk entities with
//! `Query::par_iter_mut`, so nothing shares mutable state and nothing depends
//! on which thread ran what. Between phases there is a barrier. [`install`]
//! is the only place that decides phase order.
//!
//! Only loaded chunks simulate. Which chunks are loaded is a function of the
//! inputs (camera moves, [`LoadPolicy`]), so a replay with the same inputs
//! loads the same chunks in the same order and stays bit-identical.
//!
//! Time is the integer [`Tick`] (see [`crate::time`]). A chunk that is not
//! loaded is frozen: its save file records the tick it was last simulated
//! (`last_ticked`), which is all a future catch-up-on-load needs.
//!
//! There is no engine here: no `App`, no `Time`, no assets. `app` wraps these
//! functions in a plugin; tests and the headless `wmc run` call them directly.

use std::io;

use bevy_ecs::prelude::*;
use bevy_ecs::schedule::{LogLevel, ScheduleBuildSettings, ScheduleLabel};

use crate::actors::{ActorMind, ActorsMut, ChunkActors, ChunkMinds, CrossScratch, systems};
use crate::reload::{self, PendingRemap, Plan};
use crate::rules::Kinds;
use crate::scenario::{Placement, Scenario, Start};
use crate::stage::worldgen::{GenParams, generate_many};
use crate::stage::{self, CHUNK_SIZE, ChunkCells, ChunkCoord, ChunkData, ChunkMeta, Pos, Stage};
use crate::store::{SavedKind, Store, WorldMeta};
use crate::time::START_TICK;

/// The facts a world is generated from: its [`Scenario`], with the starts
/// resolved against the loaded rules. Saved in the world header (the
/// starts by kind name); resolved again at every open.
#[derive(Resource, Debug, Clone, PartialEq)]
pub struct SimConfig {
    pub seed: u64,
    pub params: GenParams,
    pub initial_width: u32,
    pub initial_height: u32,
    /// Where kinds start, by name, as the scenario says.
    pub starts: Vec<Start>,
    /// `starts` resolved against the loaded kind table: what worldgen places.
    pub placement: Placement,
}

/// Ticks since the world began; see [`crate::time`] for the calendar.
#[derive(Resource, Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Tick(pub u64);

/// The schedule that is one sim tick. Run it with [`step`]; never through
/// the engine's fixed timestep (speed is an `app` concern).
#[derive(ScheduleLabel, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SimTick;

/// Phases of a tick, in order (`docs/ACTORS.md` §1). Systems go in one of
/// these; systems in the same phase that touch the same data must be
/// ordered explicitly.
#[derive(SystemSet, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Phase {
    /// Cell systems: scent fades.
    Simulate,
    /// Every due actor runs its program; writes own minds + intents.
    Think,
    /// Own chunk: intents into key order, WAKE consumed, bites recorded.
    Resolve,
    /// Damage, deaths, food by share, then take/give, sequentially in coordinate order.
    Exchange,
    /// Own-chunk resolution: claims, die/become/drink/move/spawn, look, result codes.
    Apply,
    /// Cross-chunk moves and spawns, sequentially in coordinate order.
    Migrate,
    /// Dead rows removed, claims reset.
    Compact,
    /// Bookkeeping: advance the tick.
    Advance,
}

/// Streaming radii, in chunks, Chebyshev distance from the focus chunk.
/// `unload > load` gives hysteresis so a camera on a boundary doesn't thrash.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LoadPolicy {
    pub load: i32,
    pub unload: i32,
}

impl Default for LoadPolicy {
    fn default() -> Self {
        Self { load: 2, unload: 4 }
    }
}

/// What one `ensure_loaded` call did. For the status line and tests.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct StreamStats {
    pub generated: usize,
    pub read: usize,
    pub unloaded: usize,
    pub written: usize,
}

// ---- schedule --------------------------------------------------------------------------

/// Give a world everything the sim needs: the `Stage` directory, a zero
/// `Tick`, the kind table, and the `SimTick` schedule with its phases.
/// Idempotent per world. The compute task pool must exist
/// (`par::init_task_pool` or an `App` with `TaskPoolPlugin`).
pub fn install(world: &mut World) {
    install_with(world, Kinds::builtin());
}

/// [`install`] with a compiled rule set instead of the built-in one.
pub fn install_with(world: &mut World, kinds: Kinds) {
    world.init_resource::<Stage>();
    world.init_resource::<Tick>();
    world.init_resource::<CrossScratch>();
    world.init_resource::<crate::actors::Tally>();
    world.init_resource::<PendingRemap>();
    world.insert_resource(kinds);
    let mut schedule = Schedule::new(SimTick);
    schedule.set_build_settings(ScheduleBuildSettings {
        // Two systems with overlapping access and no explicit order would run
        // in an order that depends on the executor: refuse to build.
        ambiguity_detection: LogLevel::Error,
        ..ScheduleBuildSettings::default()
    });
    schedule.configure_sets(
        (
            Phase::Simulate,
            Phase::Think,
            Phase::Resolve,
            Phase::Exchange,
            Phase::Apply,
            Phase::Migrate,
            Phase::Compact,
            Phase::Advance,
        )
            .chain(),
    );
    schedule.add_systems((
        crate::stage::scent::scent_decay.in_set(Phase::Simulate),
        systems::think.in_set(Phase::Think),
        systems::resolve.in_set(Phase::Resolve),
        systems::exchange.in_set(Phase::Exchange),
        systems::apply.in_set(Phase::Apply),
        systems::migrate.in_set(Phase::Migrate),
        systems::compact.in_set(Phase::Compact),
        systems::tally.in_set(Phase::Advance),
        advance_tick.in_set(Phase::Advance),
    ));
    world.add_schedule(schedule);
}

fn advance_tick(mut tick: ResMut<Tick>) {
    tick.0 += 1;
}

/// Advance one tick.
pub fn step(world: &mut World) {
    world.run_schedule(SimTick);
}

/// The current tick.
pub fn tick(world: &World) -> u64 {
    world.resource::<Tick>().0
}

// ---- creation ----------------------------------------------------------------------------

/// Turn an installed world into a fresh one made from `scenario`, its
/// initial region loaded. Same scenario and rules => bit-identical world.
/// An error, and nothing changed, if the scenario's starts do not fit the
/// rules ([`Placement::resolve`]).
pub fn create(world: &mut World, scenario: &Scenario) -> Result<(), String> {
    let placement = Placement::resolve(
        &scenario.starts,
        world.resource::<Kinds>(),
        scenario.seed,
        &scenario.params,
    )?;
    world.insert_resource(SimConfig {
        seed: scenario.seed,
        params: scenario.params,
        initial_width: scenario.width,
        initial_height: scenario.height,
        starts: scenario.starts.clone(),
        placement,
    });
    world.insert_resource(Tick(START_TICK));
    let cx = i32::try_from(scenario.width.div_ceil(CHUNK_SIZE as u32)).expect("width");
    let cy = i32::try_from(scenario.height.div_ceil(CHUNK_SIZE as u32)).expect("height");
    let coords: Vec<ChunkCoord> = (0..cy)
        .flat_map(|y| (0..cx).map(move |x| ChunkCoord::new(x, y)))
        .collect();
    load_chunks(world, &coords, None).expect("no store, no io");
    Ok(())
}

/// A standalone world (no `App`) with the built-in rules: task pool,
/// [`install`], [`create`]. For tests and benches: panics if the scenario
/// starts a kind the built-in rules do not define.
pub fn new_world(scenario: &Scenario) -> World {
    new_world_with(scenario, Kinds::builtin()).expect("the scenario fits the built-in rules")
}

/// [`new_world`] with a compiled rule set; an error if the scenario does
/// not fit it.
pub fn new_world_with(scenario: &Scenario, kinds: Kinds) -> Result<World, String> {
    crate::par::init_task_pool();
    let mut world = World::new();
    install_with(&mut world, kinds);
    create(&mut world, scenario)?;
    Ok(world)
}

/// Turn an installed world into the saved one in `store`. Nothing is loaded
/// yet: call [`ensure_loaded`] around the camera. `Ok(false)` if the store
/// holds no world.
///
/// The save's kinds are matched to the loaded rules **by name**. Rules
/// that number kinds, needs, mems, states or scent channels differently
/// (another set of packs, a newer version of one) are fine: every chunk
/// read from the store is remapped as it loads ([`reload::Plan`]), and the
/// first write to the store moves the whole directory over ([`settle`]).
/// Opening never writes. A save with a kind the rules do not define, or
/// one that moved between standing and ground cover, is refused.
pub fn open(world: &mut World, store: &Store) -> io::Result<bool> {
    let Some(m) = store.read_meta()? else {
        return Ok(false);
    };
    let bad = |msg: String| io::Error::new(io::ErrorKind::InvalidData, msg);
    let kinds = world.resource::<Kinds>();
    let missing: Vec<&str> = m
        .kinds
        .iter()
        .filter(|k| kinds.by_name(&k.name).is_none())
        .map(|k| k.name.as_str())
        .collect();
    if !missing.is_empty() {
        return Err(bad(format!(
            "the save has kinds the loaded rules do not define: {}",
            missing.join(", ")
        )));
    }
    let remap = if SavedKind::table(kinds) == m.kinds && kinds.scents == m.scents {
        None
    } else {
        Some(Plan::between(&m.kinds, &m.scents, kinds).map_err(bad)?)
    };
    let placement = Placement::resolve(&m.starts, kinds, m.seed, &m.params)
        .map_err(|e| bad(format!("the save's starts: {e}")))?;
    world.insert_resource(SimConfig {
        seed: m.seed,
        params: m.params,
        initial_width: m.initial_width,
        initial_height: m.initial_height,
        starts: m.starts,
        placement,
    });
    world.insert_resource(Tick(m.tick));
    world.insert_resource(PendingRemap(remap));
    Ok(true)
}

/// Move a save opened under other rules over to the loaded ones (see
/// [`open`]): every chunk file through the pending plan, then the world
/// file with the loaded kind table. Runs before the first write to the
/// store, so the directory never mixes two numberings. Returns the chunk
/// files rewritten (none when nothing was pending).
pub fn settle(world: &mut World, store: &Store) -> io::Result<usize> {
    let Some(plan) = world
        .get_resource::<PendingRemap>()
        .and_then(|p| p.0.clone())
    else {
        return Ok(0);
    };
    let n = reload::rewrite_saved(store, &plan, &[], &mut []).map_err(io::Error::other)?;
    store.write_meta(&meta(world))?;
    world.resource_mut::<PendingRemap>().0 = None;
    Ok(n)
}

/// A standalone world opened from `store`; `Ok(None)` if it holds no world.
pub fn open_world(store: &Store) -> io::Result<Option<World>> {
    open_world_with(store, Kinds::builtin())
}

/// [`open_world`] with a compiled rule set.
pub fn open_world_with(store: &Store, kinds: Kinds) -> io::Result<Option<World>> {
    crate::par::init_task_pool();
    let mut world = World::new();
    install_with(&mut world, kinds);
    Ok(open(&mut world, store)?.then_some(world))
}

/// The world header for `world` as it is now.
pub fn meta(world: &World) -> WorldMeta {
    let c = world.resource::<SimConfig>();
    let kinds = world.resource::<Kinds>();
    WorldMeta {
        seed: c.seed,
        tick: tick(world),
        initial_width: c.initial_width,
        initial_height: c.initial_height,
        params: c.params,
        starts: c.starts.clone(),
        map: None,
        kinds: SavedKind::table(kinds),
        scents: kinds.scents.clone(),
        packs: kinds.debug.packs.clone(),
        rules_hash: kinds.hash,
    }
}

/// Write one loaded chunk to the store.
fn write_chunk(
    world: &World,
    store: &Store,
    coord: ChunkCoord,
    e: Entity,
    now: u64,
) -> io::Result<()> {
    store.write_chunk(
        coord,
        world.get::<ChunkCells>(e).expect("chunk has cells"),
        world.get::<ChunkActors>(e).expect("chunk has actors"),
        world.get::<ChunkMinds>(e).expect("chunk has minds"),
        now,
    )
}

// ---- persistence ------------------------------------------------------------------------

/// Write metadata and every dirty chunk; dirty flags are cleared.
pub fn save(world: &mut World, store: &Store) -> io::Result<usize> {
    settle(world, store)?;
    store.write_meta(&meta(world))?;
    let now = tick(world);
    let active: Vec<(ChunkCoord, Entity)> = world.resource::<Stage>().active().to_vec();
    let mut written = 0;
    for (coord, e) in active {
        let dirty = world.get::<ChunkMeta>(e).expect("chunk has meta").dirty;
        if !dirty {
            continue;
        }
        write_chunk(world, store, coord, e, now)?;
        let mut m = world.get_mut::<ChunkMeta>(e).expect("chunk has meta");
        m.dirty = false;
        m.last_ticked = now;
        written += 1;
    }
    Ok(written)
}

/// Bring the chunks around `focus` into memory and drop far ones.
/// Load order is coordinate order; unload order likewise; both are
/// independent of thread count. Without a store, dirty chunks are never
/// unloaded (nothing could bring them back).
pub fn ensure_loaded(
    world: &mut World,
    focus: Pos,
    policy: LoadPolicy,
    store: Option<&Store>,
) -> io::Result<StreamStats> {
    assert!(
        policy.unload >= policy.load,
        "unload radius must be >= load radius"
    );
    let (fc, _) = focus.split();
    let mut stats = StreamStats::default();

    // Load.
    let mut wanted = Vec::new();
    {
        let stage = world.resource::<Stage>();
        for y in fc.y - policy.load..=fc.y + policy.load {
            for x in fc.x - policy.load..=fc.x + policy.load {
                let c = ChunkCoord::new(x, y);
                if !stage.is_loaded(c) {
                    wanted.push(c);
                }
            }
        }
    }
    let (g, r) = load_chunks(world, &wanted, store)?;
    stats.generated = g;
    stats.read = r;

    // Unload.
    let now = tick(world);
    let far: Vec<(ChunkCoord, Entity)> = world
        .resource::<Stage>()
        .active()
        .iter()
        .copied()
        .filter(|(c, _)| (c.x - fc.x).abs() > policy.unload || (c.y - fc.y).abs() > policy.unload)
        .collect();
    for (c, e) in far {
        let dirty = world.get::<ChunkMeta>(e).expect("chunk has meta").dirty;
        match (dirty, store) {
            (true, Some(st)) => {
                settle(world, st)?;
                write_chunk(world, st, c, e, now)?;
                stats.written += 1;
            }
            (true, None) => continue,
            (false, _) => {}
        }
        stage::remove(world, c);
        stats.unloaded += 1;
    }
    Ok(stats)
}

/// Load `coords` (none may be loaded already): from the store when saved
/// there, generated otherwise. Returns `(generated, read)`.
///
/// Actor rows are stamped here. A loaded chunk was frozen from the tick it
/// was written (`last_ticked`) until now: its rows' `last_think` and `born`
/// shift forward by that interval, so no need decays and nobody ages while
/// off screen (decision 29), and a reopen at the save tick is bit-identical
/// to never stopping. A generated chunk's rows are born now, needs full,
/// and the chunk is **dirty** from the start: its rows are state (their
/// birth tick, and every think from now on), so it is saved on unload,
/// never regenerated.
fn load_chunks(
    world: &mut World,
    coords: &[ChunkCoord],
    store: Option<&Store>,
) -> io::Result<(usize, usize)> {
    let now = tick(world);
    // A save opened under other rules: its rows are checked against the
    // kind table they were written with, then remapped.
    let remap = world.resource::<PendingRemap>().0.clone();
    let nkinds = remap
        .as_ref()
        .map_or(world.resource::<Kinds>().len(), Plan::old_kinds);
    let mut to_gen = Vec::with_capacity(coords.len());
    let mut read = 0;
    for &c in coords {
        match store.map(|s| s.read_chunk(c)).transpose()?.flatten() {
            Some(mut saved) => {
                saved.data.validate(nkinds).map_err(|e| {
                    io::Error::new(io::ErrorKind::InvalidData, format!("chunk {c:?}: {e}"))
                })?;
                if let Some(plan) = &remap {
                    let d = &mut saved.data;
                    plan.apply(&mut d.cells, &mut d.actors.rows, &mut d.minds.rows, &mut []);
                }
                let frozen = now.wrapping_sub(saved.last_ticked) as u32;
                for m in &mut saved.data.minds.rows {
                    m.last_think = m.last_think.wrapping_add(frozen);
                    m.born = m.born.wrapping_add(frozen);
                }
                stage::insert(world, c, saved.data, false, saved.last_ticked);
                read += 1;
            }
            None => to_gen.push(c),
        }
    }
    // Every chunk is a pure function of (seed, coord, placement): generated
    // in parallel, spawned in coordinate order. Its rows are born now, needs
    // full.
    let chunks: Vec<ChunkData> = {
        let (c, kinds) = (world.resource::<SimConfig>(), world.resource::<Kinds>());
        let mut chunks = generate_many(c.seed, &c.params, &c.placement, &to_gen);
        for data in &mut chunks {
            for (p, m) in data.actors.rows.iter().zip(&mut data.minds.rows) {
                *m = systems::newborn(kinds, p.kind, m.uid, now);
            }
        }
        chunks
    };
    for (c, data) in to_gen.iter().zip(chunks) {
        let inhabited = !data.actors.rows.is_empty();
        stage::insert(world, *c, data, inhabited, now);
    }
    Ok((to_gen.len(), read))
}

/// Re-run the think of whoever is at `p` (standing, else ground cover)
/// against the world as it is, with a trace: what the next step would have
/// it decide (`wmc why`). Changes nothing. `None` if nobody is there or the
/// chunk is not loaded.
pub fn explain(world: &World, p: Pos) -> Option<systems::Explained> {
    let kinds = world.resource::<Kinds>();
    let stage = world.resource::<Stage>();
    let tick = world.resource::<Tick>().0;
    let seed = world.resource::<SimConfig>().seed;
    let (cc, i) = p.split();
    let e = stage.entity(cc)?;
    let cells = world.get::<ChunkCells>(e)?;
    let (_, slot) = cells.occupant[i].unpack().or(cells.cover[i].unpack())?;
    let row = *world.get::<ChunkActors>(e)?.rows.get(usize::from(slot))?;
    let mind = *world.get::<ChunkMinds>(e)?.rows.get(usize::from(slot))?;
    let mut chunks = [None; 9];
    for (k, s) in chunks.iter_mut().enumerate() {
        let (ox, oy) = ((k % 3) as i32 - 1, (k / 3) as i32 - 1);
        if let Some(e) = stage.entity(ChunkCoord::new(cc.x + ox, cc.y + oy))
            && let (Some(c), Some(a)) = (world.get::<ChunkCells>(e), world.get::<ChunkActors>(e))
        {
            *s = Some((c, a));
        }
    }
    let halo = crate::rules::vm::Halo {
        chunks,
        tags: &kinds.tag_bits,
        family_end: &kinds.family_end,
    };
    Some(systems::explain(
        kinds, tick, seed, &halo, cc, slot, &row, &mind,
    ))
}

/// Where the actor with this `uid` is, if it is loaded. A scan of every
/// row: for tools, not for the tick.
pub fn find_uid(world: &mut World, uid: u64) -> Option<Pos> {
    world
        .query::<(&ChunkCoord, &ChunkActors, &ChunkMinds)>()
        .iter(world)
        .find_map(|(c, a, m)| {
            m.rows
                .iter()
                .position(|m| m.uid == uid)
                .map(|i| c.cell(usize::from(a.rows[i].cell)))
        })
}

/// Put an actor of `kind` with `mind` on the cell at `p` (in the cover
/// layer for a `cover` kind), for tests and tools (worldgen and `spawn` are
/// the in-game ways). `false` if the chunk is not loaded or the cell is not
/// walkable or its layer is taken. Marks the chunk dirty. Not for use
/// inside a tick.
pub fn place_actor(world: &mut World, p: Pos, kind: u16, mind: ActorMind) -> bool {
    let (cc, i) = p.split();
    let Some(e) = world.resource::<Stage>().entity(cc) else {
        return false;
    };
    let cover = world.resource::<Kinds>().def(kind).cover;
    let mut q = world.query::<(
        &mut ChunkCells,
        &mut ChunkActors,
        &mut ChunkMinds,
        &mut ChunkMeta,
    )>();
    let Ok((mut cells, mut pubs, mut minds, mut meta)) = q.get_mut(world, e) else {
        return false;
    };
    let layer = if cover { &cells.cover } else { &cells.occupant };
    if !cells.walkable(i) || !layer[i].is_none() {
        return false;
    }
    let mut actors = ActorsMut {
        pubs: &mut pubs.rows,
        minds: &mut minds.rows,
        cells: &mut cells,
    };
    if cover {
        actors.push_cover(i, kind, mind);
    } else {
        actors.push(i, kind, mind);
    }
    meta.dirty = true;
    true
}

/// Checksum of all loaded state, for determinism tests and bug reports.
/// The rules are an input: their hash is folded in.
pub fn checksum(world: &mut World) -> u64 {
    let t = tick(world);
    let rules = world.resource::<Kinds>().hash;
    crate::rng::splitmix64(crate::rng::splitmix64(stage::checksum(world) ^ t) ^ rules)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stage::{Feature, Ground};

    /// 150 x 70 cells, nobody placed: the streaming and save tests count
    /// clean chunks (an inhabited chunk is dirty by design), and most tests
    /// put their actors by hand.
    fn cfg(seed: u64) -> Scenario {
        Scenario {
            seed,
            width: 150,
            height: 70,
            ..Scenario::default()
        }
    }

    /// [`cfg`] with the built-in scenario's starts.
    fn populated(seed: u64) -> Scenario {
        Scenario {
            starts: Scenario::builtin().starts,
            ..cfg(seed)
        }
    }

    fn starts(text: &str) -> Vec<Start> {
        Scenario::parse("t.scenario", text).unwrap().starts
    }

    /// A need's slot in `kind`, by name: slots follow the kind's traits.
    fn slot(kinds: &Kinds, kind: u16, need: &str) -> usize {
        kinds.def(kind).need_named(need).unwrap()
    }

    fn tmp_store(name: &str) -> Store {
        let dir =
            std::env::temp_dir().join(format!("wmc-world-test-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        Store::open(dir).unwrap()
    }

    fn get(world: &World, p: Pos) -> Option<crate::stage::Cell> {
        let (cc, i) = p.split();
        stage::chunk(world, cc).map(|c| crate::stage::Cell {
            ground: c.ground[i],
            feature: c.feature[i],
            occupant: c.occupant[i],
            cover: c.cover[i],
            scent: std::array::from_fn(|j| c.scent[j][i]),
        })
    }

    #[test]
    fn new_world_covers_initial_region_in_whole_chunks() {
        let w = new_world(&cfg(1));
        assert_eq!(w.resource::<Stage>().loaded_count(), 3 * 2);
        assert!(get(&w, Pos::new(191, 127)).is_some());
        assert!(get(&w, Pos::new(192, 0)).is_none());
        assert!(get(&w, Pos::new(-1, 0)).is_none());
    }

    #[test]
    fn new_world_starts_at_dawn() {
        let mut w = new_world(&cfg(5));
        assert_eq!(tick(&w), START_TICK);
        assert_eq!(crate::time::Clock::at(tick(&w)).to_string(), "day 0 06:00");
        for m in w.query::<&ChunkMeta>().iter(&w) {
            assert_eq!(m.last_ticked, START_TICK);
        }
    }

    #[test]
    fn step_advances_tick_and_changes_checksum() {
        let mut w = new_world(&cfg(5));
        let c0 = checksum(&mut w);
        step(&mut w);
        assert_eq!(tick(&w), START_TICK + 1);
        assert_ne!(checksum(&mut w), c0);
    }

    fn checksum_after(ticks: u64) -> u64 {
        let mut w = new_world(&populated(77));
        for _ in 0..ticks {
            step(&mut w);
        }
        checksum(&mut w)
    }

    /// Same seed, same number of ticks, same checksum, run twice in one
    /// process. The thread-count half of the gate is
    /// `crates/app/tests/determinism.rs` (`WMC_THREADS=1` vs default on the
    /// real binary): Bevy's compute pool is process-wide.
    #[test]
    fn stepping_is_reproducible() {
        let one = checksum_after(200);
        assert_eq!(one, checksum_after(200));
        assert_ne!(one, checksum_after(199));
    }

    #[test]
    fn streaming_loads_generates_and_unloads_clean_chunks() {
        let mut w = new_world(&cfg(9));
        let policy = LoadPolicy { load: 1, unload: 2 };
        let s = ensure_loaded(&mut w, Pos::new(-500, -500), policy, None).unwrap();
        assert_eq!(s.generated, 9);
        assert_eq!(s.read, 0);
        assert_eq!(s.unloaded, 6); // the initial region is far away and clean
        assert_eq!(w.resource::<Stage>().loaded_count(), 9);
        // Second call at the same place is a no-op.
        let s = ensure_loaded(&mut w, Pos::new(-500, -500), policy, None).unwrap();
        assert_eq!(s, StreamStats::default());
        // Regenerated chunks are identical to the originals.
        let wide = LoadPolicy { load: 2, unload: 2 };
        ensure_loaded(&mut w, Pos::new(70, 30), wide, None).unwrap();
        // Focus chunk (1,0), radius 2 => x in -1..=3, y in -2..=2, minus the far ones.
        assert!(w.resource::<Stage>().loaded_count() > 6);
        let mut fresh = new_world(&cfg(9));
        ensure_loaded(&mut fresh, Pos::new(70, 30), wide, None).unwrap();
        assert_eq!(stage::checksum(&mut w), stage::checksum(&mut fresh));
        // Entities come and go: the ECS holds exactly the loaded set.
        let n = w.query::<&ChunkCells>().iter(&w).count();
        assert_eq!(n, w.resource::<Stage>().loaded_count());
    }

    #[test]
    fn inhabited_chunks_are_dirty_and_round_trip_through_the_store() {
        let mut w = new_world(&populated(21));
        let rows: usize = w
            .query::<&ChunkActors>()
            .iter(&w)
            .map(|a| a.rows.len())
            .sum();
        assert!(rows > 0);
        let born = START_TICK as u32;
        for (m, meta) in w.query::<(&ChunkMinds, &ChunkMeta)>().iter(&w) {
            assert_eq!(meta.dirty, !m.rows.is_empty());
            assert!(
                m.rows
                    .iter()
                    .all(|r| r.born == born && r.last_think == born)
            );
        }
        // Save everything, reopen at the same tick, load the same region:
        // bit-identical.
        let store = tmp_store("inhabited");
        step(&mut w);
        step(&mut w);
        let expect = checksum(&mut w);
        let n = save(&mut w, &store).unwrap();
        assert_eq!(n, 6, "every inhabited chunk is written");
        let policy = LoadPolicy { load: 1, unload: 1 };
        let prune = |back: &mut World| {
            let row2: Vec<ChunkCoord> = back
                .resource::<Stage>()
                .loaded_coords()
                .filter(|c| c.y == 2)
                .collect();
            for c in row2 {
                stage::remove(back, c);
            }
        };
        let mut back = open_world(&store).unwrap().unwrap();
        let s = ensure_loaded(&mut back, Pos::new(64, 64), policy, Some(&store)).unwrap();
        assert_eq!((s.read, s.generated), (6, 3));
        prune(&mut back);
        assert_eq!(checksum(&mut back), expect);
        // Reopened later: the rows were frozen meanwhile, so their clocks
        // shift by exactly the frozen interval (here: 3 ticks).
        let mut later = open_world(&store).unwrap().unwrap();
        for _ in 0..3 {
            step(&mut later);
        }
        ensure_loaded(&mut later, Pos::new(64, 64), policy, Some(&store)).unwrap();
        prune(&mut later);
        let saved_at = tick(&w) as u32;
        for m in later.query::<&ChunkMinds>().iter(&later) {
            assert!(
                m.rows
                    .iter()
                    .all(|r| r.born == born + 3 && r.last_think + 2 >= saved_at + 3)
            );
        }
        assert_ne!(checksum(&mut later), expect);
        // A save from a build with other kinds is refused.
        let mut m = meta(&w);
        m.kinds[1].name = "gremlin".into();
        store.write_meta(&m).unwrap();
        assert!(
            open_world(&store)
                .unwrap_err()
                .to_string()
                .contains("kinds")
        );
        std::fs::remove_dir_all(store.dir()).unwrap();
    }

    fn count_kinds(w: &mut World) -> Vec<usize> {
        let n = w.resource::<Kinds>().len();
        let mut counts = vec![0; n];
        for a in w.query::<&ChunkActors>().iter(w) {
            for r in &a.rows {
                counts[usize::from(r.kind)] += 1;
            }
        }
        counts
    }

    /// Seeds become trees, trees drop seeds: the forest changes and spreads,
    /// only onto free walkable cells, with every row invariant intact.
    #[test]
    fn a_forest_grows_and_spreads() {
        // The plants and the trait library they are written with.
        let plants = || {
            let files: Vec<(&str, &str)> = crate::rules::builtin::FILES
                .iter()
                .copied()
                .filter(|f| f.0 == "plants.rules" || f.0 == "lib.rules")
                .collect();
            crate::rules::compile_files(&files).unwrap()
        };
        let (seed_kind, tree_kind) = (
            plants().by_name("seed").unwrap().id,
            plants().by_name("tree").unwrap().id,
        );
        let mut w = new_world_with(
            &Scenario {
                width: 128,
                height: 128,
                starts: starts("start seed 1 / 100"),
                ..cfg(31)
            },
            plants(),
        )
        .unwrap();
        let start = count_kinds(&mut w);
        assert!(start[usize::from(seed_kind)] > 0 && start[usize::from(tree_kind)] == 0);
        for _ in 0..crate::time::days(4) {
            step(&mut w);
        }
        let end = count_kinds(&mut w);
        assert!(
            end[usize::from(tree_kind)] > 0,
            "seeds by water became trees: {end:?}"
        );
        assert!(
            end[usize::from(seed_kind)] + end[usize::from(tree_kind)]
                != start[usize::from(seed_kind)],
            "seeds died away from water and trees dropped new ones: {start:?} -> {end:?}"
        );
        let kinds = w.resource::<Kinds>().len();
        for (cells, a, m) in w
            .query::<(&ChunkCells, &ChunkActors, &ChunkMinds)>()
            .iter(&w)
        {
            crate::actors::validate(cells, &a.rows, &m.rows, kinds).unwrap();
            for r in &a.rows {
                assert!(cells.walkable(usize::from(r.cell)));
            }
            for mind in &m.rows {
                assert!(mind.needs[0] > 0, "a living actor has water");
            }
        }
        // Reproducible from scratch after thousands of ticks.
        let mut again = new_world_with(
            &Scenario {
                width: 128,
                height: 128,
                starts: starts("start seed 1 / 100"),
                ..cfg(31)
            },
            plants(),
        )
        .unwrap();
        for _ in 0..crate::time::days(4) {
            step(&mut again);
        }
        assert_eq!(checksum(&mut w), checksum(&mut again));
        assert_eq!(count_kinds(&mut again), end);
    }

    /// Save at T, reopen, step to T+N: the same as never stopping (the
    /// freeze stamp is the reload tick, which here is the save tick).
    #[test]
    fn reload_mid_run_continues_identically() {
        let store = tmp_store("mid-run");
        let mut w = new_world(&populated(8));
        let half = crate::time::hours(30);
        for _ in 0..half {
            step(&mut w);
        }
        save(&mut w, &store).unwrap();
        let mut back = open_world(&store).unwrap().unwrap();
        ensure_loaded(
            &mut back,
            Pos::new(64, 64),
            LoadPolicy { load: 1, unload: 1 },
            Some(&store),
        )
        .unwrap();
        let row2: Vec<ChunkCoord> = back
            .resource::<Stage>()
            .loaded_coords()
            .filter(|c| c.y == 2)
            .collect();
        for c in row2 {
            stage::remove(&mut back, c);
        }
        assert_eq!(checksum(&mut back), checksum(&mut w));
        for _ in 0..half {
            step(&mut w);
            step(&mut back);
        }
        assert_eq!(checksum(&mut back), checksum(&mut w));
        assert_eq!(count_kinds(&mut back), count_kinds(&mut w));
        std::fs::remove_dir_all(store.dir()).unwrap();
    }

    fn check_invariants(w: &mut World) {
        let kinds = w.resource::<Kinds>().len();
        for (cells, a, m) in w
            .query::<(&ChunkCells, &ChunkActors, &ChunkMinds)>()
            .iter(w)
        {
            crate::actors::validate(cells, &a.rows, &m.rows, kinds).unwrap();
            for r in &a.rows {
                assert!(cells.walkable(usize::from(r.cell)));
            }
        }
    }

    /// Chickens wander, drink and cross chunk borders for a game day with
    /// every row invariant intact, and two fresh worlds agree.
    #[test]
    fn chickens_wander_drink_and_cross_borders() {
        use crate::rules::CHICKEN;
        let cfg = Scenario {
            width: 128,
            height: 128,
            ..populated(17)
        };
        let mut w = new_world(&cfg);
        let chickens_at = |w: &mut World| -> Vec<(ChunkCoord, u64, u16)> {
            let mut v = Vec::new();
            for (c, a, m) in w
                .query::<(&ChunkCoord, &ChunkActors, &ChunkMinds)>()
                .iter(w)
            {
                for (r, mind) in a.rows.iter().zip(&m.rows) {
                    if r.kind == CHICKEN {
                        v.push((*c, mind.uid, r.cell));
                    }
                }
            }
            v.sort_unstable_by_key(|(c, uid, cell)| (c.key(), *uid, *cell));
            v
        };
        let start = chickens_at(&mut w);
        assert!(start.len() > 10, "{}", start.len());
        let mut crossed = 0;
        let mut moved = 0;
        let mut prev = start.clone();
        let rounds = crate::time::days(1) / 256;
        for _ in 0..rounds {
            for _ in 0..256 {
                step(&mut w);
            }
            check_invariants(&mut w);
            let now = chickens_at(&mut w);
            for &(c, uid, cell) in &now {
                if let Some(&(pc, _, pcell)) = prev.iter().find(|(_, u, _)| *u == uid) {
                    crossed += usize::from(pc != c);
                    moved += usize::from(pc != c || pcell != cell);
                }
            }
            prev = now;
        }
        let end = chickens_at(&mut w);
        assert!(moved > start.len(), "chickens walk: {moved} moves");
        assert!(crossed > 0, "some chicken crossed a chunk border");
        // Foxes, thirst and hunger thin them out; some live a whole day.
        assert!(!end.is_empty(), "no chicken survived a day");
        // Every live row has every vital need above zero (a need at zero is
        // death at the next think, and damage kills at once).
        let kinds = w.resource::<Kinds>().clone();
        for (a, m) in w.query::<(&ChunkActors, &ChunkMinds)>().iter(&w) {
            for (r, mind) in a.rows.iter().zip(&m.rows) {
                for (i, need) in kinds.def(r.kind).needs.iter().enumerate() {
                    if need.vital && !need.decays {
                        assert!(
                            mind.needs[i] > 0,
                            "{} {}",
                            kinds.def(r.kind).name,
                            need.name
                        );
                    }
                }
            }
        }
        let mut again = new_world(&cfg);
        for _ in 0..rounds * 256 {
            step(&mut again);
        }
        assert_eq!(checksum(&mut again), checksum(&mut w));
    }

    /// The Migrate phase on its own: a border crossing, two contenders for
    /// one cell, an unloaded neighbour.
    #[test]
    fn migrate_moves_rows_between_chunks_by_key() {
        use crate::actors::systems::{
            Effect, EffectKind, Outbox, Scratch, compact, intent_key, migrate, newborn,
        };
        use crate::rules::CHICKEN;
        use bevy_ecs::system::RunSystemOnce;
        crate::par::init_task_pool();
        let mut w = World::new();
        install(&mut w);
        create(
            &mut w,
            &Scenario {
                width: 1,
                height: 1,
                ..cfg(1)
            },
        )
        .unwrap();
        // Two bare chunks side by side; a chicken at the east edge of the
        // west one and two more that want the same cell of the east one.
        let kinds = w.resource::<Kinds>().clone();
        let mut west = ChunkData::default();
        let a_slot =
            west.actors_mut()
                .push(10 * 64 + 63, CHICKEN, newborn(&kinds, CHICKEN, 100, 0));
        let b_slot =
            west.actors_mut()
                .push(20 * 64 + 63, CHICKEN, newborn(&kinds, CHICKEN, 200, 0));
        let c_slot =
            west.actors_mut()
                .push(30 * 64 + 63, CHICKEN, newborn(&kinds, CHICKEN, 300, 0));
        stage::remove(&mut w, ChunkCoord::new(0, 0));
        let west_e = stage::insert(&mut w, ChunkCoord::new(0, 0), west, true, 0);
        let east_e = stage::insert(&mut w, ChunkCoord::new(1, 0), ChunkData::default(), true, 0);
        let tick = tick(&w);
        let key = |uid| intent_key(uid, tick);
        let mut ob = w.get_mut::<Outbox>(west_e).unwrap();
        ob.list.push(Effect {
            key: key(100),
            slot: a_slot,
            what: EffectKind::Move,
            to: ChunkCoord::new(1, 0),
            cell: 10 * 64,
        });
        ob.list.push(Effect {
            key: key(200),
            slot: b_slot,
            what: EffectKind::Move,
            to: ChunkCoord::new(1, 0),
            cell: 25 * 64,
        });
        ob.list.push(Effect {
            key: key(300),
            slot: c_slot,
            what: EffectKind::Move,
            to: ChunkCoord::new(1, 0),
            cell: 25 * 64,
        });
        // A fourth wants an unloaded chunk.
        ob.list.push(Effect {
            key: key(300),
            slot: c_slot,
            what: EffectKind::Spawn {
                kind: CHICKEN,
                with: [0; 2],
            },
            to: ChunkCoord::new(0, 1),
            cell: 0,
        });
        w.run_system_once(migrate).unwrap();
        let east = w.get::<ChunkActors>(east_e).unwrap().rows.clone();
        let east_minds = w.get::<ChunkMinds>(east_e).unwrap().rows.clone();
        assert_eq!(east.len(), 2, "a and the lower-key contender arrived");
        let by_uid = |uid: u64| east_minds.iter().position(|m| m.uid == uid);
        let a = by_uid(100).expect("a crossed");
        assert_eq!(east[a].cell, 10 * 64);
        assert_eq!(
            east_minds[a].events & crate::rules::vm::result::MASK,
            crate::rules::vm::result::OK
        );
        let winner = if key(200) < key(300) { 200 } else { 300 };
        let wslot = by_uid(winner).expect("the lower key crossed");
        assert_eq!(east[wslot].cell, 25 * 64);
        assert_eq!(
            w.get::<ChunkCells>(east_e).unwrap().occupant[25 * 64],
            crate::stage::ActorId::pack(CHICKEN, wslot as u16)
        );
        let west_minds = w.get::<ChunkMinds>(west_e).unwrap().rows.clone();
        let west_pubs = w.get::<ChunkActors>(west_e).unwrap().rows.clone();
        let loser_slot = if winner == 200 { c_slot } else { b_slot };
        assert_eq!(
            west_minds[usize::from(loser_slot)].events & crate::rules::vm::result::MASK,
            crate::rules::vm::result::BLOCKED
        );
        assert!(west_pubs[usize::from(a_slot)].flags & crate::actors::flags::DEAD != 0);
        assert!(w.get::<ChunkCells>(west_e).unwrap().occupant[10 * 64 + 63].is_none());
        assert_eq!(w.get::<Scratch>(west_e).unwrap().deaths, 2);
        w.run_system_once(compact).unwrap();
        let west_pubs = w.get::<ChunkActors>(west_e).unwrap().rows.clone();
        assert_eq!(west_pubs.len(), 1, "the loser stays");
        check_invariants(&mut w);
        // Same tick again: the filled cells are touched, so a second mover
        // into them is BLOCKED even though the cell looks free... it is
        // not free (occupied), and a vacated one is touched: neither enterable.
        let mut ob = w.get_mut::<Outbox>(west_e).unwrap();
        ob.list.push(Effect {
            key: key(999),
            slot: 0,
            what: EffectKind::Move,
            to: ChunkCoord::new(1, 0),
            cell: 10 * 64,
        });
        w.run_system_once(migrate).unwrap();
        let west_minds = w.get::<ChunkMinds>(west_e).unwrap().rows.clone();
        assert_eq!(
            west_minds[0].events & crate::rules::vm::result::MASK,
            crate::rules::vm::result::BLOCKED
        );
        assert_eq!(w.get::<ChunkActors>(east_e).unwrap().rows.len(), 2);
    }

    /// Every cell of the loaded chunks becomes plain soil: scenarios place
    /// actors where they want them.
    fn flatten(w: &mut World) {
        let coords: Vec<ChunkCoord> = w.resource::<Stage>().loaded_coords().collect();
        for c in coords {
            let mut cells = stage::chunk_mut(w, c).unwrap();
            cells.ground = [Ground::Soil; crate::stage::CHUNK_CELLS];
            cells.feature = [Feature::None; crate::stage::CHUNK_CELLS];
        }
    }

    /// `(uid, kind, pos, mind)` of every row, in uid order.
    fn rows(w: &mut World) -> Vec<(u64, u16, Pos, ActorMind)> {
        let mut v = Vec::new();
        for (c, a, m) in w
            .query::<(&ChunkCoord, &ChunkActors, &ChunkMinds)>()
            .iter(w)
        {
            for (r, mind) in a.rows.iter().zip(&m.rows) {
                v.push((mind.uid, r.kind, c.cell(usize::from(r.cell)), *mind));
            }
        }
        v.sort_unstable_by_key(|r| r.0);
        v
    }

    /// Resolve + Exchange on hand-made intents: two eaters on one victim
    /// (one across a chunk border) kill it, each fed the share it took, in
    /// key order, overkill feeding nobody; a hit wounds, wakes and points
    /// `hurt_dir` at the biter; a bite on an empty cell misses; a far bite,
    /// or one on a kind without health, is refused.
    #[test]
    fn bites_take_health_in_key_order_and_feed_by_share() {
        use crate::actors::flags;
        use crate::actors::systems::{Intent, Intents, Scratch, exchange, newborn, resolve};
        use crate::rules::vm::{Action, dir_index, result};
        use crate::time::hours;
        use bevy_ecs::system::RunSystemOnce;
        let kinds = crate::rules::compile(
            "t",
            "kind wolf  { bite 10 food 1d need food max 2d vital need health max 30 decay 0 vital }
             kind sheep { food 12h need health max 15 decay 0 vital }
             kind stone { }",
        )
        .unwrap();
        let (wolf, sheep, stone) = (0u16, 1u16, 2u16);
        let cfg = Scenario {
            width: 128,
            height: 64,
            ..cfg(2)
        };
        let mut w = new_world_with(&cfg, kinds.clone()).unwrap();
        flatten(&mut w);
        let now = tick(&w);
        let mut put = |x, y, kind, uid| {
            let mut m = newborn(&kinds, kind, uid, now);
            if kind == wolf {
                m.needs[0] = hours(6) as i32;
            }
            assert!(place_actor(&mut w, Pos::new(x, y), kind, m));
        };
        // Chunk (0, 0), slots in placement order.
        put(63, 10, sheep, 1); // 0: S, eaten from both sides
        put(62, 10, wolf, 10); // 1: A, eats east
        put(20, 20, sheep, 2); // 2: T, only hit
        put(21, 20, wolf, 12); // 3: F, hits west
        put(30, 30, wolf, 13); // 4: C, eats an empty cell
        put(40, 30, wolf, 14); // 5: D, eats two cells away
        put(51, 30, stone, 3); // 6: a stone
        put(50, 30, wolf, 15); // 7: E, eats the stone
        // Chunk (1, 0).
        put(64, 10, wolf, 11); // 0: B, eats west across the border
        let e0 = w.resource::<Stage>().entity(ChunkCoord::new(0, 0)).unwrap();
        let e1 = w.resource::<Stage>().entity(ChunkCoord::new(1, 0)).unwrap();
        let it = |slot, key, action, dx| Intent {
            slot,
            key,
            action,
            kind: 0,
            dx,
            dy: 0,
            look: None,
            signal: None,
            amount: 0,
            with: [0; 2],
            mark: None,
            used: 0,
            trapped: false,
        };
        w.get_mut::<ChunkActors>(e0).unwrap().rows[1].flags |= flags::WAKE;
        w.get_mut::<Intents>(e0).unwrap().list.extend([
            it(1, 5, Action::Eat, 1),
            it(3, 7, Action::Hit, -1),
            it(4, 20, Action::Eat, 1),
            it(5, 21, Action::Eat, 2),
            it(7, 22, Action::Eat, 1),
        ]);
        w.get_mut::<Intents>(e1)
            .unwrap()
            .list
            .push(it(0, 3, Action::Eat, -1));
        w.run_system_once(resolve).unwrap();
        w.run_system_once(exchange).unwrap();

        let pubs0 = w.get::<ChunkActors>(e0).unwrap().rows.clone();
        let minds0 = w.get::<ChunkMinds>(e0).unwrap().rows.clone();
        let minds1 = w.get::<ChunkMinds>(e1).unwrap().rows.clone();
        let res = |m: &ActorMind| m.events & result::MASK;
        assert!(pubs0[0].flags & flags::DEAD != 0, "S took 20 of 15");
        assert!(w.get::<ChunkCells>(e0).unwrap().occupant[10 * 64 + 63].is_none());
        assert_eq!(w.get::<Scratch>(e0).unwrap().deaths, 1);
        // B (key 3) bites first: 10 of 15 health, 10/15 of 12h = 8h. A (key
        // 5) takes the 5 left: 5/15 of 12h = 4h. Their other 5 is overkill.
        assert_eq!(
            minds1[0].needs[0],
            (hours(6) + hours(8)) as i32,
            "B, the lower key, took 10 of 15"
        );
        assert_eq!(
            minds0[1].needs[0],
            (hours(6) + hours(4)) as i32,
            "A took the 5 left"
        );
        assert_eq!((res(&minds0[1]), res(&minds1[0])), (result::OK, result::OK));
        assert_eq!(
            pubs0[1].flags & flags::WAKE,
            0,
            "A's think consumed its wake"
        );
        // T: wounded, woken, pointed at F (east of it).
        assert_eq!(minds0[2].needs[0], 5);
        assert_eq!((minds0[2].hurt, minds0[2].hurt_dir), (10, dir_index(1, 0)));
        assert_ne!(pubs0[2].flags & flags::WAKE, 0);
        assert_eq!(pubs0[2].flags & flags::DEAD, 0);
        assert_eq!(res(&minds0[3]), result::OK);
        assert_eq!(res(&minds0[4]), result::MISSED);
        assert_eq!(res(&minds0[5]), result::REFUSED);
        assert_eq!(res(&minds0[7]), result::REFUSED);
    }

    /// The real rules: a hungry fox next to a penned chicken bites twice
    /// and eats it, in its own chunk and across a chunk border.
    #[test]
    fn a_fox_eats_a_cornered_chicken_in_its_chunk_and_across_a_border() {
        use crate::actors::systems::newborn;
        use crate::rules::{CHICKEN, FOX};
        use crate::time::hours;
        let kinds = Kinds::builtin();
        let cfg = Scenario {
            width: 128,
            height: 64,
            ..cfg(3)
        };
        let mut w = new_world_with(&cfg, kinds.clone()).unwrap();
        flatten(&mut w);
        let now = tick(&w);
        for (chicken, fox, cuid, fuid) in [
            (Pos::new(20, 20), Pos::new(19, 20), 0xC0, 0xF0),
            (Pos::new(64, 40), Pos::new(63, 40), 0xC1, 0xF1),
        ] {
            // A pen of rocks around the chicken, open only where the fox is.
            for dy in -1..=1 {
                for dx in -1..=1 {
                    let p = Pos::new(chicken.x + dx, chicken.y + dy);
                    if (dx, dy) != (0, 0) && p != fox {
                        let (cc, i) = p.split();
                        stage::chunk_mut(&mut w, cc).unwrap().feature[i] = Feature::Rock;
                    }
                }
            }
            assert!(place_actor(
                &mut w,
                chicken,
                CHICKEN,
                newborn(&kinds, CHICKEN, cuid, now)
            ));
            // Starving: a chicken is a day's food (12h a bite), so it takes
            // both bites before the fox is fed.
            let mut hungry = newborn(&kinds, FOX, fuid, now);
            hungry.needs[slot(&kinds, FOX, "food")] = hours(2) as i32;
            assert!(place_actor(&mut w, fox, FOX, hungry));
        }
        let mut wounded = false;
        for _ in 0..64 {
            step(&mut w);
            check_invariants(&mut w);
            wounded |= rows(&mut w)
                .iter()
                .any(|r| r.1 == CHICKEN && r.3.needs[slot(&kinds, CHICKEN, "health")] == 10);
        }
        let all = rows(&mut w);
        assert!(wounded, "a chicken was bitten once before it died");
        assert!(
            all.iter().all(|r| r.1 != CHICKEN),
            "both chickens were eaten"
        );
        let foxes: Vec<_> = all.iter().filter(|r| r.1 == FOX).collect();
        assert_eq!(foxes.len(), 2);
        for f in foxes {
            assert!(
                f.3.needs[slot(&kinds, FOX, "food")] > hours(23) as i32,
                "fox {:x} ate a whole chicken: {:?}",
                f.0,
                f.3.needs
            );
        }
    }

    /// `take` and `give` move a need between neighbours by name, in key
    /// order in Exchange, in a chunk and across a border; a taken-from
    /// actor wakes and sees `taken`; a target without the need refuses;
    /// `spawn ... with` seeds the child's first two mem slots.
    #[test]
    fn take_and_give_move_needs_between_neighbours() {
        use crate::actors::systems::newborn;
        use crate::rules::compile;
        use crate::rules::vm::result;
        use crate::time::hours;
        let kinds = compile(
            "t.rules",
            "kind pot { glyph \"P\"  tags store  cadence 1024
               need honey max 10h decay 0
               mem seen
               when taken => { seen += 1  idle } }
             kind stone { glyph \"S\"  tags store  cadence 1024 }
             kind bee { glyph \"b\"  cadence 1  sight 2
               need honey max 3h decay 0
               mem mode, got, r1, gave, r2
               when mode == 0 and nearest store within 1 as p => { mode = 1  take p honey 2h }
               when mode == 1 => { mode = 2  got = honey  r1 = result }
               when mode == 2 and nearest store within 1 as p => { mode = 3  give p honey 5h }
               when mode == 3 => { mode = 4  gave = honey  r2 = result }
               when mode == 4 and nearest free within 1 as c => { mode = 5  spawn bee at c with (mode = 7, got = x) }
               when true => idle }",
        )
        .unwrap();
        let (pot, stone, bee) = (0, 1, 2);
        let cfg = Scenario {
            width: 128,
            height: 64,
            ..cfg(9)
        };
        let mut w = new_world_with(&cfg, kinds.clone()).unwrap();
        flatten(&mut w);
        let now = tick(&w);
        let mut full = newborn(&kinds, pot, 0x90, now);
        full.needs[0] = hours(5) as i32;
        // In one chunk; across the border at x = 64; next to a stone.
        for (store, sp, bp, uid) in [
            (pot, Pos::new(10, 10), Pos::new(11, 10), 0xB1),
            (pot, Pos::new(64, 20), Pos::new(63, 20), 0xB2),
            (stone, Pos::new(30, 40), Pos::new(31, 40), 0xB3),
        ] {
            let m = ActorMind {
                uid: uid + 0x100,
                ..full
            };
            let m = if store == stone {
                newborn(&kinds, stone, uid + 0x100, now)
            } else {
                m
            };
            assert!(place_actor(&mut w, sp, store, m));
            let mut b = newborn(&kinds, bee, uid, now);
            b.needs[0] = 0;
            assert!(place_actor(&mut w, bp, bee, b));
        }
        for _ in 0..8 {
            step(&mut w);
            check_invariants(&mut w);
        }
        let all = rows(&mut w);
        let find = |uid: u64| all.iter().find(|r| r.0 == uid).unwrap().3;
        for uid in [0xB1, 0xB2] {
            let (b, p) = (find(uid), find(uid + 0x100));
            // Took 2h of the pot's 5h, gave back all 2h it had (asked 5h).
            assert_eq!(&b.mem[..5], &[5, hours(2) as i32, 1, 0, 1], "bee {uid:x}");
            assert_eq!(b.needs[0], 0);
            assert_eq!(p.needs[0], hours(5) as i32, "pot of {uid:x}");
            assert_eq!(p.mem[0], 1, "the pot woke and saw `taken` once");
        }
        let b3 = find(0xB3);
        assert_eq!(
            b3.mem[2],
            i32::from(result::REFUSED),
            "a stone has no honey"
        );
        // Three children, each born with (7, parent's x).
        let kids: Vec<_> = all
            .iter()
            .filter(|r| r.1 == bee && r.3.mem[0] == 7)
            .collect();
        assert_eq!(kids.len(), 3);
        for k in kids {
            assert!([11, 63, 31].contains(&k.3.mem[1]), "{:?}", k.3.mem);
        }
    }

    /// `mark` raises scent on the actor's cell, `scent(ch)` reads it, a
    /// step later `sniff` finds it (in the chunk and across a border); the
    /// scent is saved with the chunk and fades to nothing in two hours; a
    /// third channel does not compile.
    #[test]
    fn marks_are_sniffed_saved_and_fade() {
        use crate::actors::systems::newborn;
        use crate::rules::compile;
        use crate::time::hours;
        let kinds = compile(
            "t.rules",
            "kind ant { glyph \"a\"  cadence 1  sight 4
               mem mode, here_s, sx, sy, far
               when mode == 0 => { mode = 1  mark trail 200  idle }
               when mode == 1 => { mode = 2  here_s = scent(trail)  idle }
               when mode == 2 => { mode = 3  move east }
               when mode == 3 and sniff trail within 3 as v => { mode = 4  sx = v.dx  sy = v.dy  far = scent(trail, v) }
               when true => idle }",
        )
        .unwrap();
        assert_eq!(kinds.scents, vec!["trail".to_string()]);
        let cfg = Scenario {
            width: 128,
            height: 64,
            ..cfg(12)
        };
        let mut w = new_world_with(&cfg, kinds.clone()).unwrap();
        flatten(&mut w);
        let now = tick(&w);
        for (x, uid) in [(20, 0xA1), (63, 0xA2)] {
            assert!(place_actor(
                &mut w,
                Pos::new(x, 30),
                0,
                newborn(&kinds, 0, uid, now)
            ));
        }
        for _ in 0..6 {
            step(&mut w);
            check_invariants(&mut w);
        }
        let all = rows(&mut w);
        for (uid, x) in [(0xA1, 20), (0xA2, 63)] {
            let a = all.iter().find(|r| r.0 == uid).unwrap();
            assert_eq!(a.2, Pos::new(x + 1, 30), "ant {uid:x} stepped east");
            let m = a.3.mem;
            assert_eq!(m[0], 4, "ant {uid:x}: {m:?}");
            assert!(m[1] > 150 && m[1] <= 200, "read its own mark: {m:?}");
            assert_eq!((m[2], m[3]), (-1, 0), "sniffed the cell it left");
            assert!(m[4] > 150, "{m:?}");
            assert!(get(&w, Pos::new(x, 30)).unwrap().scent[0] > 150);
        }
        // Saved and read back with the chunk.
        let store = tmp_store("scent");
        let c = ChunkCoord::new(0, 0);
        let cells = stage::chunk(&w, c).unwrap().clone();
        let e = w.resource::<Stage>().entity(c).unwrap();
        let actors = w.get::<ChunkActors>(e).unwrap().clone();
        let minds = w.get::<ChunkMinds>(e).unwrap().clone();
        store
            .write_chunk(c, &cells, &actors, &minds, tick(&w))
            .unwrap();
        let back = store.read_chunk(c).unwrap().unwrap();
        assert_eq!(back.data.cells, cells);
        std::fs::remove_dir_all(store.dir()).unwrap();
        // Two hours later it is gone.
        for _ in 0..hours(2) {
            step(&mut w);
        }
        assert_eq!(
            get(&w, Pos::new(20, 30)).unwrap().scent,
            [0; crate::stage::SCENT_CHANNELS]
        );

        let e = compile(
            "t.rules",
            "kind a { when true => { mark s1 1  mark s2 1  mark s3 1  mark s4 1  mark s5 1 } }",
        )
        .unwrap_err();
        assert!(e.to_string().contains("at most 4 scents"), "{e}");
    }

    /// The acceptance test of the social primitives (docs/ACTORS.md §11
    /// step 6): a hive 25 cells from a flower patch it cannot see. Its bees
    /// wander until one finds the flowers, sips them (`take`), flies home
    /// marking a trail, gives its crop (`give`) and dances; others read
    /// the dance (`look_of`/`signal_of` through `bee:2`) and fly to the
    /// flowers; visited flowers set seed; bees bring home more than the
    /// hive spent on them.
    #[test]
    fn bees_find_flowers_dance_and_fill_the_hive() {
        use crate::actors::systems::newborn;
        use crate::actors::{Tally, life};
        use crate::rules::{BEE, FLOWER, HIVE};
        use crate::time::hours;
        let kinds = Kinds::builtin();
        let cfg = Scenario {
            width: 128,
            height: 128,
            ..cfg(21)
        };
        let mut w = new_world_with(&cfg, kinds.clone()).unwrap();
        flatten(&mut w);
        // A pond at (86..90, 60..64) and flowers around it; the hive 25 west.
        for y in 60..64 {
            for x in 86..90 {
                let (cc, i) = Pos::new(x, y).split();
                stage::chunk_mut(&mut w, cc).unwrap().ground[i] = Ground::Water;
            }
        }
        let now = tick(&w);
        let mut n = 0u64;
        for y in 56..68 {
            for x in 82..94 {
                let edge = !(85..91).contains(&x) || !(59..65).contains(&y);
                if edge && x % 3 == 0 && y % 3 == 0 {
                    n += 1;
                    assert!(place_actor(
                        &mut w,
                        Pos::new(x, y),
                        FLOWER,
                        newborn(&kinds, FLOWER, 0xF000 + n, now)
                    ));
                }
            }
        }
        let hive_at = Pos::new(58, 62);
        assert!(place_actor(
            &mut w,
            hive_at,
            HIVE,
            newborn(&kinds, HIVE, 0x41, now)
        ));
        let (mut danced, mut recruited, mut trail) = (false, false, false);
        for t in 0..hours(12) {
            step(&mut w);
            if t % 64 != 0 {
                continue;
            }
            check_invariants(&mut w);
            for (_, kind, _, m) in rows(&mut w) {
                match kind {
                    BEE if m.state == 3 => danced = true,    // DANCE
                    BEE if m.state == 1 => recruited = true, // GOTO
                    _ => {}
                }
            }
            trail |= (60..84).any(|x| get(&w, Pos::new(x, 62)).unwrap().scent[0] > 0);
        }
        let all = rows(&mut w);
        let hive = all.iter().find(|r| r.1 == HIVE).unwrap().3;
        let bees = all.iter().filter(|r| r.1 == BEE).count();
        let tally = w.resource::<Tally>();
        assert!(bees >= 5, "the hive spawned bees: {bees}");
        assert!(danced, "a bee came home full and danced");
        assert!(recruited, "a bee followed a dance");
        assert!(trail, "the way home was marked");
        assert!(
            tally.get(FLOWER, life::BORN) >= 1,
            "visited flowers set seed"
        );
        // Stores now + 2h per bee spent - the 2 days it started with.
        let born = tally.get(BEE, life::BORN) as i32;
        let brought = hive.needs[0] + born * hours(2) as i32 - hours(48) as i32;
        assert!(
            brought > hours(4) as i32,
            "bees brought nectar home: {brought} ticks' worth, {born} bees"
        );
    }

    /// Thinks, ops and traps are counted per kind; a think that runs out of
    /// fuel idles and the next one sees `trapped`.
    #[test]
    fn thinks_ops_and_traps_are_counted_per_kind() {
        use crate::actors::systems::newborn;
        use crate::actors::{Tally, life};
        use crate::rules::compile;
        let kinds = compile(
            "t.rules",
            "kind spin { cadence 1  mem saw
               when trapped => { saw += 1  idle }
               when true => { while true { } } }
             kind calm { cadence 2  when true => idle }",
        )
        .unwrap();
        let cfg = Scenario {
            width: 64,
            height: 64,
            ..cfg(3)
        };
        let mut w = new_world_with(&cfg, kinds.clone()).unwrap();
        flatten(&mut w);
        let now = tick(&w);
        assert!(place_actor(
            &mut w,
            Pos::new(5, 5),
            0,
            newborn(&kinds, 0, 0x51, now)
        ));
        assert!(place_actor(
            &mut w,
            Pos::new(9, 5),
            1,
            newborn(&kinds, 1, 0x52, now)
        ));
        for _ in 0..10 {
            step(&mut w);
        }
        let t = w.resource::<Tally>();
        assert_eq!(t.get(0, life::THINKS), 10);
        // Every other think traps (the one after sees `trapped` and idles).
        assert_eq!(t.get(0, life::TRAPS), 5);
        assert!(
            t.get(0, life::OPS) >= 5 * 500,
            "a trapped think spends its fuel"
        );
        assert_eq!((t.get(1, life::THINKS), t.get(1, life::TRAPS)), (5, 0));
        assert_eq!(t.get(1, life::OPS), 5 * 4); // Push 1, Jz, Act, EndRule
        let spin = rows(&mut w).into_iter().find(|r| r.0 == 0x51).unwrap();
        assert_eq!(spin.3.mem[0], 5);
    }

    /// `explain` replays an actor's next think with a trace and changes
    /// nothing; the step that follows does what it said.
    #[test]
    fn explain_replays_the_next_think_without_changing_the_world() {
        use crate::actors::systems::newborn;
        use crate::rules::CHICKEN;
        use crate::rules::vm::{Action, result};
        use crate::time::{hours, minutes};
        let kinds = Kinds::builtin();
        let cfg = Scenario {
            width: 64,
            height: 64,
            ..cfg(8)
        };
        let mut w = new_world_with(&cfg, kinds.clone()).unwrap();
        flatten(&mut w);
        {
            let (cc, i) = Pos::new(20, 20).split();
            stage::chunk_mut(&mut w, cc).unwrap().ground[i] = Ground::Water;
        }
        let now = tick(&w);
        let mut thirsty = newborn(&kinds, CHICKEN, 0xC1, now);
        thirsty.needs[slot(&kinds, CHICKEN, "water")] = minutes(20) as i32;
        let at = Pos::new(21, 20);
        assert!(place_actor(&mut w, at, CHICKEN, thirsty));
        assert!(explain(&w, Pos::new(30, 30)).is_none(), "nobody there");
        while !explain(&w, at).unwrap().due {
            step(&mut w);
        }
        let sum = checksum(&mut w);
        let e = explain(&w, at).unwrap();
        assert_eq!(checksum(&mut w), sum, "explaining changes nothing");
        assert_eq!(e.intent.action, Action::Drink);
        assert_eq!((e.intent.dx, e.intent.dy, e.intent.kind), (-1, 0, 1));
        let fired: Vec<_> = kinds
            .debug
            .rules
            .iter()
            .filter(|r| r.kind == CHICKEN && e.trace.iter().any(|s| s.pc == r.body_pc))
            .collect();
        assert_eq!(fired.len(), 1, "{fired:?}");
        assert!(fired[0].text.contains("drink w"), "{}", fired[0].text);
        assert_eq!(
            kinds.debug.files[usize::from(fired[0].file)],
            "animals.rules"
        );
        assert_eq!(
            e.trace.last().unwrap().op.code,
            crate::rules::vm::OpCode::EndRule
        );
        step(&mut w);
        let c = rows(&mut w).into_iter().find(|r| r.0 == 0xC1).unwrap();
        assert_eq!(
            c.3.needs[slot(&kinds, CHICKEN, "water")],
            hours(4) as i32,
            "it drank"
        );
        assert_eq!(c.3.events & result::MASK, result::OK);
        assert_eq!(find_uid(&mut w, 0xC1), Some(at));
    }

    /// Hot reload maps rows by name: kinds renumbered, a kind gone (its rows
    /// dropped), needs and mems moved, added and removed, a scent channel
    /// gone; a saved chunk that is not loaded is rewritten, so it loads
    /// under the new rules and the save reopens with them.
    #[test]
    fn reload_remaps_rows_by_name_in_memory_and_on_disk() {
        use crate::actors::systems::newborn;
        use crate::reload::reload_rules;
        use crate::rules::compile;
        let a = compile(
            "a.rules",
            "kind a { glyph \"a\"  need p max 100 decay 0  need q max 50 decay 0  mem m1, m2
               when true => { mark s1 10  idle } }
             kind b { glyph \"b\"  when true => idle }
             kind c { glyph \"c\"  cover  need health max 4 decay 0 vital }",
        )
        .unwrap();
        let b = compile(
            "b.rules",
            "kind d { glyph \"d\"  when true => idle }
             kind a { glyph \"A\"  need q max 40 decay 0  need r max 7 decay 0  mem m2, m3
               when true => idle }
             kind c { glyph \"c\"  cover  need health max 4 decay 0 vital }",
        )
        .unwrap();
        let cfg = Scenario {
            width: 128,
            height: 64,
            ..cfg(2)
        };
        let store = tmp_store("reload");
        let mut w = new_world_with(&cfg, a.clone()).unwrap();
        flatten(&mut w);
        let now = tick(&w);
        let mut m = newborn(&a, 0, 0xA1, now);
        (m.needs[0], m.needs[1], m.mem[0], m.mem[1]) = (90, 45, 11, 22);
        assert!(place_actor(&mut w, Pos::new(5, 5), 0, m));
        assert!(place_actor(
            &mut w,
            Pos::new(6, 5),
            1,
            newborn(&a, 1, 0xB1, now)
        ));
        assert!(place_actor(
            &mut w,
            Pos::new(7, 5),
            2,
            newborn(&a, 2, 0xC1, now)
        ));
        assert!(place_actor(
            &mut w,
            Pos::new(70, 5),
            0,
            ActorMind { uid: 0xA2, ..m }
        ));
        for _ in 0..8 {
            step(&mut w); // kind a thinks every 8 ticks
        }
        assert!(get(&w, Pos::new(5, 5)).unwrap().scent[0] > 0);
        save(&mut w, &store).unwrap();
        // Chunk (1, 0) goes to disk and out of memory.
        ensure_loaded(
            &mut w,
            Pos::new(5, 5),
            LoadPolicy { load: 0, unload: 0 },
            Some(&store),
        )
        .unwrap();
        assert!(
            w.resource::<Stage>()
                .entity(ChunkCoord::new(1, 0))
                .is_none()
        );

        let bad = compile("c.rules", "kind c { glyph \"c\" }").unwrap();
        let e = reload_rules(&mut w, Some(&store), bad).unwrap_err();
        assert!(e.contains("ground cover"), "{e}");

        let r = reload_rules(&mut w, Some(&store), b.clone()).unwrap();
        assert_eq!(r.added, vec!["d".to_string()]);
        assert_eq!(r.removed, vec![("b".to_string(), 1)]);
        assert_eq!(r.rewritten, 2, "both chunk files, the loaded one's too");
        assert_eq!(r.hash, b.hash);
        check_invariants(&mut w);
        let all = rows(&mut w);
        assert_eq!(all.len(), 2, "b's row was dropped");
        let a1 = all.iter().find(|r| r.0 == 0xA1).unwrap();
        assert_eq!(a1.1, 1, "a is kind 1 now");
        assert_eq!(
            &a1.3.needs[..2],
            &[40, 7],
            "q clamped to its new max, r new"
        );
        assert_eq!(&a1.3.mem[..2], &[22, 0], "m2 kept, m3 new");
        let c1 = all.iter().find(|r| r.0 == 0xC1).unwrap();
        assert_eq!(c1.1, 2);
        let cell = get(&w, Pos::new(7, 5)).unwrap();
        assert_eq!(cell.cover.unpack().map(|(k, _)| k), Some(2));
        let none = [0; crate::stage::SCENT_CHANNELS];
        assert_eq!(cell.scent, none);
        assert_eq!(get(&w, Pos::new(5, 5)).unwrap().scent, none, "s1 is gone");
        // The chunk on disk loads under the new rules.
        ensure_loaded(
            &mut w,
            Pos::new(70, 5),
            LoadPolicy { load: 1, unload: 3 },
            Some(&store),
        )
        .unwrap();
        check_invariants(&mut w);
        let a2 = rows(&mut w).into_iter().find(|r| r.0 == 0xA2).unwrap();
        assert_eq!((a2.1, &a2.3.needs[..2]), (1, &[40, 7][..]));
        // And the save reopens with them.
        save(&mut w, &store).unwrap();
        assert!(open_world_with(&store, b).unwrap().is_some());
        assert!(open_world_with(&store, a).is_err());
        std::fs::remove_dir_all(store.dir()).unwrap();
    }

    /// `drink` needs adjacent water: from three cells away it is refused.
    #[test]
    fn drinking_needs_adjacent_water() {
        use crate::actors::systems::newborn;
        use crate::rules::compile;
        use crate::rules::vm::result;
        let kinds = compile(
            "t.rules",
            "kind d { cadence 1  sight 4  need water max 1h  mem r
               when r == 0 and nearest water within 4 as w => { r = 9  drink w }
               when r == 9 => { r = result  idle } }",
        )
        .unwrap();
        let cfg = Scenario {
            width: 64,
            height: 64,
            ..cfg(1)
        };
        let mut w = new_world_with(&cfg, kinds.clone()).unwrap();
        flatten(&mut w);
        let (cc, i) = Pos::new(13, 10).split();
        stage::chunk_mut(&mut w, cc).unwrap().ground[i] = Ground::Water;
        let mut m = newborn(&kinds, 0, 0xD1, tick(&w));
        m.needs[0] = 100;
        assert!(place_actor(&mut w, Pos::new(10, 10), 0, m));
        for _ in 0..3 {
            step(&mut w);
        }
        let d = rows(&mut w).into_iter().find(|r| r.0 == 0xD1).unwrap();
        assert_eq!(d.3.mem[0], i32::from(result::REFUSED));
        assert!(d.3.needs[0] < 100, "no refill from afar: {}", d.3.needs[0]);
    }

    /// A kind built from traits with `inherit` and a member sub behaves
    /// exactly like the same kind written flat: same rows, tick for tick
    /// (only the rules hash differs).
    #[test]
    fn a_kind_built_from_traits_walks_the_same_path_as_the_flat_one() {
        use crate::actors::systems::newborn;
        use crate::rules::compile;
        let flat = compile(
            "flat.rules",
            "kind hen { glyph \"h\"  cadence 2  sight 4
               need water max 4h vital  need food max 1d vital
               mem heading, detour
               when blocked => { heading = rand(8) + 1  detour = 2 }
               when detour > 0 => { detour -= 1  move dir(heading) }
               when water < 3h and nearest water within 1 as w => drink w
               when true => { heading = heading % 8 + 1  move dir(heading) } }",
        )
        .unwrap();
        let built = compile(
            "built.rules",
            "trait walker { mem heading, detour
               sub wander() { heading = heading % 8 + 1  move dir(heading) }
               when blocked => { heading = rand(8) + 1  detour = 2 }
               when detour > 0 => { detour -= 1  move dir(heading) } }
             trait drinker(t) { need water max 4h vital
               when water < t and nearest water within 1 as w => drink w }
             kind hen extends walker, drinker(3h) { glyph \"h\"  cadence 2  sight 4
               need food max 1d vital
               inherit walker
               inherit drinker
               when true => wander() }",
        )
        .unwrap();
        assert_eq!(flat.defs[0].needs, built.defs[0].needs);
        assert_eq!(flat.defs[0].mems, built.defs[0].mems);
        assert_ne!(flat.hash, built.hash);
        let world = |kinds: &Kinds| {
            let cfg = Scenario {
                width: 64,
                height: 64,
                ..cfg(14)
            };
            let mut w = new_world_with(&cfg, kinds.clone()).unwrap();
            flatten(&mut w);
            for y in 30..34 {
                for x in 30..34 {
                    let (cc, i) = Pos::new(x, y).split();
                    stage::chunk_mut(&mut w, cc).unwrap().ground[i] = Ground::Water;
                }
            }
            let now = tick(&w);
            for (i, (x, y)) in [(10, 10), (28, 31), (40, 50)].into_iter().enumerate() {
                let m = newborn(kinds, 0, 0x100 + i as u64, now);
                assert!(place_actor(&mut w, Pos::new(x, y), 0, m));
            }
            for _ in 0..3000 {
                step(&mut w);
            }
            w
        };
        let (mut a, mut b) = (world(&flat), world(&built));
        assert_eq!(rows(&mut a), rows(&mut b));
        assert_eq!(stage::checksum(&mut a), stage::checksum(&mut b));
    }

    /// Grass is ground cover: a hungry chicken walks onto a patch, stands on
    /// a tuft (both layers of one cell taken) and grazes it underfoot, a
    /// quarter tuft a bite, until the tuft is gone; the patch never blocks.
    #[test]
    fn chickens_walk_onto_grass_and_graze_it() {
        use crate::actors::systems::newborn;
        use crate::actors::{Tally, life};
        use crate::rules::{CHICKEN, GRASS};
        use crate::time::hours;
        let kinds = Kinds::builtin();
        let cfg = Scenario {
            width: 64,
            height: 64,
            ..cfg(6)
        };
        let mut w = new_world_with(&cfg, kinds.clone()).unwrap();
        flatten(&mut w);
        let now = tick(&w);
        for y in 20..25 {
            for x in 20..25 {
                let mut tuft = newborn(&kinds, GRASS, (x * 100 + y) as u64, now);
                tuft.needs[slot(&kinds, GRASS, "water")] = hours(40) as i32; // no water here: keep it alive
                assert!(place_actor(&mut w, Pos::new(x, y), GRASS, tuft));
            }
        }
        let mut hungry = newborn(&kinds, CHICKEN, 0xC0, now);
        hungry.needs[slot(&kinds, CHICKEN, "food")] = hours(6) as i32;
        assert!(place_actor(&mut w, Pos::new(17, 22), CHICKEN, hungry));
        check_invariants(&mut w);
        let mut stood_on_grass = false;
        for _ in 0..200 {
            step(&mut w);
            check_invariants(&mut w);
            let c = rows(&mut w).into_iter().find(|r| r.0 == 0xC0).unwrap();
            let (cc, i) = c.2.split();
            let cell = &stage::chunk(&w, cc).unwrap();
            stood_on_grass |= cell.cover[i].unpack().map(|(k, _)| k) == Some(GRASS);
        }
        assert!(stood_on_grass, "the chicken walked onto the patch");
        let c = rows(&mut w).into_iter().find(|r| r.0 == 0xC0).unwrap();
        assert!(
            c.3.needs[slot(&kinds, CHICKEN, "food")] > hours(7) as i32,
            "grazing fed it: {:?}",
            c.3.needs
        );
        let tally = w.resource::<Tally>();
        assert!(
            tally.get(GRASS, life::EATEN) >= 1,
            "a tuft was grazed to the ground"
        );
        assert_eq!(tally.get(CHICKEN, life::EATEN), 0);
    }

    /// A hungry chicken eats the seeds around it; an egg hatches into a
    /// chick after eight hours, keeping its uid, with its needs full.
    #[test]
    fn chickens_graze_seeds_and_eggs_hatch() {
        use crate::actors::systems::newborn;
        use crate::rules::{CHICK, CHICKEN, EGG, SEED};
        use crate::time::{hours, minutes};
        let kinds = Kinds::builtin();
        let cfg = Scenario {
            width: 64,
            height: 64,
            ..cfg(4)
        };
        let mut w = new_world_with(&cfg, kinds.clone()).unwrap();
        flatten(&mut w);
        let now = tick(&w);
        let mut hungry = newborn(&kinds, CHICKEN, 0xC0, now);
        hungry.needs[slot(&kinds, CHICKEN, "food")] = hours(6) as i32;
        assert!(place_actor(&mut w, Pos::new(30, 30), CHICKEN, hungry));
        for (x, uid) in [(31, 0x51), (33, 0x53)] {
            assert!(place_actor(
                &mut w,
                Pos::new(x, 30),
                SEED,
                newborn(&kinds, SEED, uid, now)
            ));
        }
        assert!(place_actor(
            &mut w,
            Pos::new(10, 10),
            EGG,
            newborn(&kinds, EGG, 0xE0, now)
        ));
        for _ in 0..64 {
            step(&mut w);
        }
        let all = rows(&mut w);
        assert!(all.iter().all(|r| r.1 != SEED), "both seeds eaten");
        let grazer = all.iter().find(|r| r.0 == 0xC0).unwrap();
        assert!(
            grazer.3.needs[slot(&kinds, CHICKEN, "food")] > (hours(12) - minutes(10)) as i32,
            "6h + two seeds of 3h: {}",
            grazer.3.needs[slot(&kinds, CHICKEN, "food")]
        );
        let mut hatched_at = None;
        while tick(&w) < now + hours(9) {
            for _ in 0..16 {
                step(&mut w);
            }
            let egg = rows(&mut w).into_iter().find(|r| r.0 == 0xE0).unwrap();
            if egg.1 == CHICK && hatched_at.is_none() {
                hatched_at = Some(tick(&w));
                let chick = kinds.def(CHICK);
                for (i, need) in chick.needs.iter().enumerate() {
                    assert!(egg.3.needs[i] > need.max - 100, "{} starts full", need.name);
                }
            }
        }
        let at = hatched_at.expect("the egg hatched");
        assert!(
            at > now + hours(8) && at <= now + hours(8) + 64 + 16,
            "{}",
            at - now
        );
    }

    /// The built-in world for two game days: every row invariant holds,
    /// chickens lay eggs, and the populations move.
    #[test]
    fn a_small_ecosystem_runs_two_days() {
        use crate::rules::{CHICKEN, EGG};
        let cfg = Scenario {
            width: 256,
            height: 256,
            ..populated(12)
        };
        let mut w = new_world(&cfg);
        let start = count_kinds(&mut w);
        let mut eggs_seen = 0;
        for _ in 0..crate::time::days(2) / 512 {
            for _ in 0..512 {
                step(&mut w);
            }
            check_invariants(&mut w);
            eggs_seen = eggs_seen.max(count_kinds(&mut w)[usize::from(EGG)]);
        }
        let end = count_kinds(&mut w);
        assert!(start[usize::from(CHICKEN)] > 20, "{start:?}");
        assert!(eggs_seen > 0, "chickens laid eggs: {start:?} -> {end:?}");
        assert_ne!(start, end);
    }

    #[test]
    fn corrupt_rows_are_refused_on_load() {
        let store = tmp_store("rows");
        let mut w = new_world(&populated(4));
        save(&mut w, &store).unwrap();
        let c = ChunkCoord::new(0, 0);
        let mut saved = store.read_chunk(c).unwrap().unwrap();
        saved.data.actors.rows[0].kind = 500; // past the kind table
        store
            .write_chunk(
                c,
                &saved.data.cells,
                &saved.data.actors,
                &saved.data.minds,
                0,
            )
            .unwrap();
        let mut back = open_world(&store).unwrap().unwrap();
        let err = ensure_loaded(
            &mut back,
            Pos::new(0, 0),
            LoadPolicy { load: 0, unload: 0 },
            Some(&store),
        )
        .unwrap_err();
        assert!(err.to_string().contains("unknown kind"), "{err}");
        std::fs::remove_dir_all(store.dir()).unwrap();
    }

    #[test]
    fn dirty_chunks_survive_unload_only_through_a_store() {
        let mut w = new_world(&cfg(3));
        let p = Pos::new(10, 10);
        let (cc, i) = p.split();
        stage::chunk_mut(&mut w, cc).unwrap().feature[i] = Feature::Rock;
        stage::chunk_mut(&mut w, cc).unwrap().ground[i] = Ground::Water;
        let policy = LoadPolicy { load: 0, unload: 0 };

        // No store: the dirty chunk stays resident.
        let s = ensure_loaded(&mut w, Pos::new(1000, 1000), policy, None).unwrap();
        assert_eq!(s.unloaded, 5);
        assert!(w.resource::<Stage>().is_loaded(cc));

        // With a store: written on unload, read back on load, bit-exact, and
        // stamped with the tick it was frozen at.
        let store = tmp_store("dirty");
        let before = stage::chunk(&w, cc).unwrap().hash();
        step(&mut w);
        step(&mut w);
        let frozen_at = tick(&w);
        let s = ensure_loaded(&mut w, Pos::new(1000, 1000), policy, Some(&store)).unwrap();
        assert_eq!((s.written, s.unloaded), (1, 1));
        assert!(!w.resource::<Stage>().is_loaded(cc));
        step(&mut w);
        let s = ensure_loaded(&mut w, p, policy, Some(&store)).unwrap();
        assert_eq!((s.read, s.generated), (1, 0));
        assert_eq!(stage::chunk(&w, cc).unwrap().hash(), before);
        let e = w.resource::<Stage>().entity(cc).unwrap();
        let m = *w.get::<ChunkMeta>(e).unwrap();
        assert!(!m.dirty);
        assert_eq!(m.last_ticked, frozen_at);
        assert_eq!(tick(&w), frozen_at + 1);
        std::fs::remove_dir_all(store.dir()).unwrap();
    }

    #[test]
    fn save_and_open_roundtrip() {
        let store = tmp_store("save");
        let mut w = new_world(&cfg(11));
        step(&mut w);
        step(&mut w);
        let (cc, i) = Pos::new(100, 60).split();
        stage::chunk_mut(&mut w, cc).unwrap().feature[i] = Feature::Rock;
        assert_eq!(save(&mut w, &store).unwrap(), 1);
        assert_eq!(save(&mut w, &store).unwrap(), 0);
        let expect = checksum(&mut w);

        let mut back = open_world(&store).unwrap().unwrap();
        assert_eq!(
            (tick(&back), back.resource::<SimConfig>().seed),
            (START_TICK + 2, 11)
        );
        assert_eq!(back.resource::<Stage>().loaded_count(), 0);
        // Load exactly the initial region again.
        ensure_loaded(
            &mut back,
            Pos::new(64, 64),
            LoadPolicy { load: 1, unload: 1 },
            Some(&store),
        )
        .unwrap();
        // (0..=2, 0..=2) minus row 2, which the original never had: prune it.
        let row2: Vec<ChunkCoord> = back
            .resource::<Stage>()
            .loaded_coords()
            .filter(|c| c.y == 2)
            .collect();
        for c in row2 {
            stage::remove(&mut back, c);
        }
        assert_eq!(checksum(&mut back), expect);
        assert_eq!(open_world(&tmp_store("empty")).unwrap().map(|_| ()), None);
        std::fs::remove_dir_all(store.dir()).unwrap();
    }

    /// The first `n` cells of the 150 x 70 region at `seed` that `want`
    /// accepts, by terrain.
    fn cells_where(seed: u64, n: usize, want: fn(Ground, Feature) -> bool) -> Vec<Pos> {
        let p = GenParams::default();
        (0..150 * 70)
            .map(|i| Pos::new(i % 150, i / 150))
            .filter(|q| {
                let (g, f) = crate::stage::worldgen::gen_cell(seed, &p, q.x, q.y);
                want(g, f)
            })
            .take(n)
            .collect()
    }

    /// A scenario's explicit starts land where it says, newborn, in their
    /// kind's layer; a start on water, on rock, or naming a kind the rules
    /// do not define refuses the world.
    #[test]
    fn explicit_starts_are_placed_and_bad_ones_are_refused() {
        use crate::rules::{CHICKEN, GRASS};
        let dry = cells_where(6, 2, |g, f| g.walkable() && !f.blocks());
        let s = Scenario {
            starts: starts(&format!(
                "start chicken at ({}, {})\nstart grass at ({}, {})",
                dry[0].x, dry[0].y, dry[1].x, dry[1].y
            )),
            ..cfg(6)
        };
        let mut w = new_world(&s);
        let all = rows(&mut w);
        assert_eq!(all.len(), 2);
        let kinds = w.resource::<Kinds>().clone();
        for (kind, at, cover) in [(CHICKEN, dry[0], false), (GRASS, dry[1], true)] {
            let c = get(&w, at).unwrap();
            let layer = if cover { c.cover } else { c.occupant };
            assert_eq!(layer.unpack().map(|(k, _)| k), Some(kind));
            let (uid, _, _, mind) = *all.iter().find(|r| r.2 == at).unwrap();
            let born = systems::newborn(&kinds, kind, uid, START_TICK);
            assert_eq!(mind, born, "born at creation, needs full");
        }
        let refused = |text: String| {
            let mut w = World::new();
            install(&mut w);
            create(
                &mut w,
                &Scenario {
                    starts: starts(&text),
                    ..cfg(6)
                },
            )
            .unwrap_err()
        };
        let wet = cells_where(6, 1, |g, f| !g.walkable() && !f.blocks())[0];
        let rock = cells_where(6, 1, |_, f| f.blocks())[0];
        let e = refused(format!("start hive at ({}, {})", wet.x, wet.y));
        assert_eq!(
            e,
            format!("`start hive at ({}, {})` is on water", wet.x, wet.y)
        );
        let e = refused(format!("start hive at ({}, {})", rock.x, rock.y));
        assert!(e.ends_with("is on rock"), "{e}");
        let e = refused("start wolf 1 / 9\nstart chicken 1 / 9\nstart gnu at (0, 0)".into());
        assert_eq!(
            e,
            "the scenario starts kinds the rules do not define: wolf, gnu"
        );
    }

    /// A save keeps its scenario's starts: chunks first generated after a
    /// reopen come out as they would have without the save, explicit
    /// starts included.
    #[test]
    fn a_saved_world_keeps_its_starts() {
        use crate::rules::HIVE;
        let p = GenParams::default();
        let far = (0..crate::stage::CHUNK_CELLS)
            .map(|i| ChunkCoord::new(-5, 14).cell(i))
            .find(|q| {
                let (g, f) = crate::stage::worldgen::gen_cell(9, &p, q.x, q.y);
                g.walkable() && !f.blocks()
            })
            .unwrap();
        let s = Scenario {
            starts: starts(&format!(
                "start chicken 1 / 50\nstart hive at ({}, {})",
                far.x, far.y
            )),
            ..cfg(9)
        };
        let store = tmp_store("starts");
        let mut w = new_world(&s);
        save(&mut w, &store).unwrap();
        let mut back = open_world(&store).unwrap().unwrap();
        assert_eq!(back.resource::<SimConfig>(), w.resource::<SimConfig>());
        let policy = LoadPolicy { load: 1, unload: 1 };
        for world in [&mut w, &mut back] {
            ensure_loaded(world, far, policy, Some(&store)).unwrap();
        }
        let c = get(&back, far).unwrap();
        assert_eq!(c.occupant.unpack().map(|(k, _)| k), Some(HIVE));
        assert_eq!(rows(&mut back), rows(&mut w));
        assert_eq!(stage::checksum(&mut back), stage::checksum(&mut w));
        std::fs::remove_dir_all(store.dir()).unwrap();
    }

    /// Hot reload resolves the scenario's starts against the new rules: a
    /// kind that moved keeps its share by name, a kind that is gone starts
    /// nowhere, and the save header keeps what is left.
    #[test]
    fn reload_resolves_the_starts_again() {
        use crate::reload::reload_rules;
        use crate::rules::compile;
        let a = compile("a.rules", "kind a { glyph \"a\" }\nkind b { glyph \"b\" }").unwrap();
        let b = compile("b.rules", "kind b { glyph \"B\" }\nkind d { glyph \"d\" }").unwrap();
        let s = Scenario {
            starts: starts("start a 1 / 4\nstart b 1 / 2"),
            ..cfg(2)
        };
        let store = tmp_store("reload-starts");
        let mut w = new_world_with(&s, a).unwrap();
        save(&mut w, &store).unwrap();
        reload_rules(&mut w, Some(&store), b.clone()).unwrap();
        let c = w.resource::<SimConfig>().clone();
        assert_eq!(c.starts, starts("start b 1 / 2"));
        // Chunks generated from now on start `b` (now kind 0), never `d`.
        let far = LoadPolicy { load: 1, unload: 1 };
        ensure_loaded(&mut w, Pos::new(-2000, 0), far, Some(&store)).unwrap();
        let n = count_kinds(&mut w);
        assert!(n[0] > 1000 && n[1] == 0, "{n:?}");
        let back = open_world_with(&store, b).unwrap().unwrap();
        assert_eq!(back.resource::<SimConfig>().starts, c.starts);
        std::fs::remove_dir_all(store.dir()).unwrap();
    }

    const HENS: &str = "kind hen {
  glyph \"h\"  cadence 4
  need food  max 1d vital
  need water max 4h
  mem steps, seen
  when true => { steps += 1  mark trail 50  move random free }
}
kind fox   { glyph \"f\"  cadence 8  when true => idle }
kind grass { glyph \"'\"  cover  need health max 4 decay 0 vital }
";

    /// Pack A (hens, a fox, grass), and A after a pack B whose file sorts
    /// first: `ant` takes kind 0 and `musk` scent channel 0, so every id
    /// A's rows and scent layers hold moves.
    fn packs_a_ab() -> (Kinds, Kinds) {
        let b = "kind ant { glyph \"a\"  when true => mark musk 9 }";
        let a = crate::rules::compile_files(&[("a/hens.rules", HENS)]).unwrap();
        let ab =
            crate::rules::compile_files(&[("b/ants.rules", b), ("a/hens.rules", HENS)]).unwrap();
        (a, ab)
    }

    type Named = (u64, String, Pos, Vec<(String, i32)>, Vec<(String, i32)>);

    /// Every loaded row by name: uid, kind, cell, needs and mems.
    fn named(w: &mut World) -> Vec<Named> {
        let kinds = w.resource::<Kinds>().clone();
        rows(w)
            .into_iter()
            .map(|(uid, k, p, m)| {
                let d = kinds.def(k);
                let needs = d
                    .needs
                    .iter()
                    .map(|n| n.name.clone())
                    .zip(m.needs)
                    .collect();
                let mems = d.mems.iter().cloned().zip(m.mem).collect();
                (uid, d.name.clone(), p, needs, mems)
            })
            .collect()
    }

    /// Every scented cell by channel name, in cell order.
    fn scents_named(w: &mut World) -> Vec<(Pos, String, u8)> {
        let names = w.resource::<Kinds>().scents.clone();
        let mut v = Vec::new();
        for (c, cells) in w.query::<(&ChunkCoord, &ChunkCells)>().iter(w) {
            for (name, ch) in names.iter().zip(&cells.scent) {
                for (i, &s) in ch.iter().enumerate().filter(|(_, s)| **s > 0) {
                    v.push((c.cell(i), name.clone(), s));
                }
            }
        }
        v.sort_by_key(|(p, n, _)| (p.y, p.x, n.clone()));
        v
    }

    /// Under pack A: two hens (one in chunk (1, 0)), a fox and a tuft, eight
    /// ticks of marking trails, saved. Its rows and scent, by name.
    fn saved_hens(store: &Store, a: &Kinds) -> (Vec<Named>, Vec<(Pos, String, u8)>) {
        use crate::actors::systems::newborn;
        let mut w = new_world_with(&cfg(3), a.clone()).unwrap();
        flatten(&mut w);
        let now = tick(&w);
        for (x, kind, uid) in [(5, 0, 0x1), (6, 1, 0x2), (7, 2, 0x3), (70, 0, 0x4)] {
            let m = newborn(a, kind, uid, now);
            assert!(place_actor(&mut w, Pos::new(x, 5), kind, m));
        }
        for _ in 0..8 {
            step(&mut w);
        }
        save(&mut w, store).unwrap();
        (named(&mut w), scents_named(&mut w))
    }

    /// A save opens under a superset of its packs that numbers everything
    /// differently: every actor keeps its cell, uid, needs and memory, every
    /// trail its channel by name, and opening writes nothing.
    #[test]
    fn a_save_opens_with_a_superset_pack_and_keeps_every_actor() {
        let (a, ab) = packs_a_ab();
        assert_eq!(
            (a.by_name("hen").unwrap().id, ab.by_name("hen").unwrap().id),
            (0, 1)
        );
        assert_eq!(
            (&a.scents[..], &ab.scents[..]),
            (
                &["trail".to_string()][..],
                &["musk".to_string(), "trail".to_string()][..]
            )
        );
        let store = tmp_store("superset");
        let (rows_a, scent_a) = saved_hens(&store, &a);
        assert_eq!(rows_a.len(), 4);
        assert!(!scent_a.is_empty());
        let mut w = open_world_with(&store, ab.clone()).unwrap().unwrap();
        let around = LoadPolicy { load: 1, unload: 1 };
        ensure_loaded(&mut w, Pos::new(64, 32), around, Some(&store)).unwrap();
        check_invariants(&mut w);
        assert_eq!(named(&mut w), rows_a);
        assert_eq!(scents_named(&mut w), scent_a);
        let meta = store.read_meta().unwrap().unwrap();
        assert_eq!(meta.kinds, SavedKind::table(&a), "opening never writes");
        std::fs::remove_dir_all(store.dir()).unwrap();
    }

    #[test]
    fn a_save_missing_a_kind_is_refused_with_the_list() {
        let (a, _) = packs_a_ab();
        let store = tmp_store("missing");
        saved_hens(&store, &a);
        let only_grass = crate::rules::compile("g.rules", "kind grass { cover }").unwrap();
        let e = open_world_with(&store, only_grass).unwrap_err().to_string();
        assert_eq!(
            e,
            "the save has kinds the loaded rules do not define: hen, fox"
        );
        let standing =
            crate::rules::compile("g.rules", "kind hen { } kind fox { } kind grass { }").unwrap();
        let e = open_world_with(&store, standing).unwrap_err().to_string();
        assert!(
            e.contains("`grass` moved between standing and ground cover"),
            "{e}"
        );
        std::fs::remove_dir_all(store.dir()).unwrap();
    }

    /// The first write to a save opened under other rules moves the whole
    /// directory over, whether it is a `save` or an unload that writes: it
    /// then reopens under the new rules with nothing left to remap, and no
    /// longer under the old ones.
    #[test]
    fn a_remapped_save_is_rewritten_on_save_and_reopens_with_the_new_pack_only() {
        let around = LoadPolicy { load: 1, unload: 1 };
        for by_unload in [false, true] {
            let (a, ab) = packs_a_ab();
            let store = tmp_store(if by_unload {
                "remap-unload"
            } else {
                "remap-save"
            });
            let (mut want, _) = saved_hens(&store, &a);
            let mut w = open_world_with(&store, ab.clone()).unwrap().unwrap();
            assert!(w.resource::<PendingRemap>().0.is_some());
            ensure_loaded(&mut w, Pos::new(64, 32), around, Some(&store)).unwrap();
            if by_unload {
                for _ in 0..8 {
                    step(&mut w); // the hens think: their chunks turn dirty
                }
                want = named(&mut w);
                let s = ensure_loaded(&mut w, Pos::new(5000, 32), around, Some(&store)).unwrap();
                assert!(s.written > 0);
            } else {
                save(&mut w, &store).unwrap();
            }
            assert!(w.resource::<PendingRemap>().0.is_none());
            let meta = store.read_meta().unwrap().unwrap();
            assert_eq!(
                (meta.kinds, meta.scents),
                (SavedKind::table(&ab), ab.scents.clone())
            );
            let mut back = open_world_with(&store, ab.clone()).unwrap().unwrap();
            assert!(
                back.resource::<PendingRemap>().0.is_none(),
                "nothing left to remap"
            );
            ensure_loaded(&mut back, Pos::new(64, 32), around, Some(&store)).unwrap();
            check_invariants(&mut back);
            assert_eq!(named(&mut back), want, "by_unload: {by_unload}");
            let e = open_world_with(&store, a).unwrap_err().to_string();
            assert!(e.ends_with("do not define: ant"), "{e}");
            std::fs::remove_dir_all(store.dir()).unwrap();
        }
    }

    #[test]
    fn schedule_refuses_ambiguous_systems() {
        // Two systems writing `Tick` in the same phase with no order between
        // them: the build must fail loudly instead of picking an order.
        crate::par::init_task_pool();
        let mut w = World::new();
        install(&mut w);
        fn a(mut t: ResMut<Tick>) {
            t.0 += 1;
        }
        fn b(mut t: ResMut<Tick>) {
            t.0 *= 2;
        }
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let mut s = Schedule::new(SimTick);
            s.set_build_settings(ScheduleBuildSettings {
                ambiguity_detection: LogLevel::Error,
                ..ScheduleBuildSettings::default()
            });
            s.add_systems((a, b));
            s.run(&mut w);
        }));
        assert!(result.is_err(), "ambiguous schedule built and ran");
    }
}
