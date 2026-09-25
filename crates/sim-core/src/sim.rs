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
use crate::rules::Kinds;
use crate::stage::worldgen::{GenParams, generate_many};
use crate::stage::{self, CHUNK_SIZE, ChunkCells, ChunkCoord, ChunkData, ChunkMeta, Pos, Stage};
use crate::store::{Store, WorldMeta};
use crate::time::START_TICK;

/// How a new world is made.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WorldConfig {
    pub seed: u64,
    /// Initially generated region, in cells, at `[0, w) x [0, h)`. Rounded up
    /// to whole chunks.
    pub width: u32,
    pub height: u32,
    pub params: GenParams,
}

/// The facts a world is generated from. Immutable once created; saved in the
/// world header.
#[derive(Resource, Debug, Clone, Copy, PartialEq)]
pub struct SimConfig {
    pub seed: u64,
    pub params: GenParams,
    pub initial_width: u32,
    pub initial_height: u32,
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
    /// Cell systems (none yet).
    Simulate,
    /// Every due actor runs its program; writes own minds + intents.
    Think,
    /// Own chunk: intents into key order, WAKE consumed, bites recorded.
    Resolve,
    /// Damage, deaths and kill credit, sequentially in coordinate order.
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

/// Turn an installed world into a fresh one with its initial region loaded.
/// Same config => bit-identical world.
pub fn create(world: &mut World, cfg: &WorldConfig) {
    world.insert_resource(SimConfig {
        seed: cfg.seed,
        params: cfg.params,
        initial_width: cfg.width,
        initial_height: cfg.height,
    });
    world.insert_resource(Tick(START_TICK));
    let cx = i32::try_from(cfg.width.div_ceil(CHUNK_SIZE as u32)).expect("width");
    let cy = i32::try_from(cfg.height.div_ceil(CHUNK_SIZE as u32)).expect("height");
    let coords: Vec<ChunkCoord> = (0..cy)
        .flat_map(|y| (0..cx).map(move |x| ChunkCoord::new(x, y)))
        .collect();
    load_chunks(world, &coords, None).expect("no store, no io");
}

/// A standalone world (no `App`): task pool, [`install`], [`create`]. For
/// tests, benches and `wmc show`.
pub fn new_world(cfg: &WorldConfig) -> World {
    new_world_with(cfg, Kinds::builtin())
}

/// [`new_world`] with a compiled rule set.
pub fn new_world_with(cfg: &WorldConfig, kinds: Kinds) -> World {
    crate::par::init_task_pool();
    let mut world = World::new();
    install_with(&mut world, kinds);
    create(&mut world, cfg);
    world
}

/// Turn an installed world into the saved one in `store`. Nothing is loaded
/// yet: call [`ensure_loaded`] around the camera. `Ok(false)` if the store
/// holds no world; an error if it was written with a different kind table
/// (its rows would mean something else here).
pub fn open(world: &mut World, store: &Store) -> io::Result<bool> {
    let Some(m) = store.read_meta()? else {
        return Ok(false);
    };
    let ours: Vec<&str> = world.resource::<Kinds>().names().collect();
    if m.kinds != ours {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("save has kinds {:?}, this build has {ours:?}", m.kinds),
        ));
    }
    world.insert_resource(SimConfig {
        seed: m.seed,
        params: m.params,
        initial_width: m.initial_width,
        initial_height: m.initial_height,
    });
    world.insert_resource(Tick(m.tick));
    Ok(true)
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

pub fn meta(world: &World) -> WorldMeta {
    let c = world.resource::<SimConfig>();
    WorldMeta {
        seed: c.seed,
        tick: tick(world),
        initial_width: c.initial_width,
        initial_height: c.initial_height,
        params: c.params,
        kinds: world
            .resource::<Kinds>()
            .names()
            .map(str::to_string)
            .collect(),
        rules_hash: world.resource::<Kinds>().hash,
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
    let nkinds = world.resource::<Kinds>().len();
    let mut to_gen = Vec::with_capacity(coords.len());
    let mut read = 0;
    for &c in coords {
        match store.map(|s| s.read_chunk(c)).transpose()?.flatten() {
            Some(mut saved) => {
                saved.data.validate(nkinds).map_err(|e| {
                    io::Error::new(io::ErrorKind::InvalidData, format!("chunk {c:?}: {e}"))
                })?;
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
    let (seed, params) = {
        let c = world.resource::<SimConfig>();
        (c.seed, c.params)
    };
    // Every chunk is a pure function of (seed, coord, kind table): generated
    // in parallel, spawned in coordinate order. Its rows are born now, needs
    // full.
    let chunks: Vec<ChunkData> = {
        let kinds = world.resource::<Kinds>();
        let mut chunks = generate_many(seed, &params, kinds, &to_gen);
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

/// Put an actor of `kind` with `mind` on the cell at `p` (in the cover
/// layer for a `cover` kind), for scenarios and tests (worldgen and `spawn`
/// are the in-game ways). `false` if the chunk is not loaded or the cell is
/// not walkable or its layer is taken. Marks the chunk dirty. Not for use
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

    fn cfg(seed: u64) -> WorldConfig {
        WorldConfig {
            seed,
            width: 150,
            height: 70,
            params: GenParams::default(),
        }
    }

    /// The built-in rules, placing nobody: the streaming and save tests
    /// below count clean chunks, and an inhabited chunk is dirty by design.
    fn bare() -> Kinds {
        Kinds::builtin().without_placement()
    }

    fn bare_world(cfg: &WorldConfig) -> World {
        new_world_with(cfg, bare())
    }

    fn open_bare(store: &Store) -> io::Result<Option<World>> {
        open_world_with(store, bare())
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
        })
    }

    #[test]
    fn new_world_covers_initial_region_in_whole_chunks() {
        let w = bare_world(&cfg(1));
        assert_eq!(w.resource::<Stage>().loaded_count(), 3 * 2);
        assert!(get(&w, Pos::new(191, 127)).is_some());
        assert!(get(&w, Pos::new(192, 0)).is_none());
        assert!(get(&w, Pos::new(-1, 0)).is_none());
    }

    #[test]
    fn new_world_starts_at_dawn() {
        let mut w = bare_world(&cfg(5));
        assert_eq!(tick(&w), START_TICK);
        assert_eq!(crate::time::Clock::at(tick(&w)).to_string(), "day 0 06:00");
        for m in w.query::<&ChunkMeta>().iter(&w) {
            assert_eq!(m.last_ticked, START_TICK);
        }
    }

    #[test]
    fn step_advances_tick_and_changes_checksum() {
        let mut w = bare_world(&cfg(5));
        let c0 = checksum(&mut w);
        step(&mut w);
        assert_eq!(tick(&w), START_TICK + 1);
        assert_ne!(checksum(&mut w), c0);
    }

    fn checksum_after(ticks: u64) -> u64 {
        let mut w = new_world(&cfg(77));
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
        let mut w = bare_world(&cfg(9));
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
        let mut fresh = bare_world(&cfg(9));
        ensure_loaded(&mut fresh, Pos::new(70, 30), wide, None).unwrap();
        assert_eq!(stage::checksum(&mut w), stage::checksum(&mut fresh));
        // Entities come and go: the ECS holds exactly the loaded set.
        let n = w.query::<&ChunkCells>().iter(&w).count();
        assert_eq!(n, w.resource::<Stage>().loaded_count());
    }

    #[test]
    fn inhabited_chunks_are_dirty_and_round_trip_through_the_store() {
        let mut w = new_world(&cfg(21));
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
        m.kinds = vec!["seed".into(), "gremlin".into()];
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
        let plants = || {
            crate::rules::compile(
                "plants.rules",
                crate::rules::builtin::FILES
                    .iter()
                    .find(|f| f.0 == "plants.rules")
                    .unwrap()
                    .1,
            )
            .unwrap()
        };
        let (seed_kind, tree_kind) = (
            plants().by_name("seed").unwrap().id,
            plants().by_name("tree").unwrap().id,
        );
        let mut w = new_world_with(
            &WorldConfig {
                width: 128,
                height: 128,
                ..cfg(31)
            },
            plants(),
        );
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
            &WorldConfig {
                width: 128,
                height: 128,
                ..cfg(31)
            },
            plants(),
        );
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
        let mut w = new_world(&cfg(8));
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
        let cfg = WorldConfig {
            width: 128,
            height: 128,
            ..cfg(17)
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
            &WorldConfig {
                width: 1,
                height: 1,
                ..cfg(1)
            },
        );
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
        let cfg = WorldConfig {
            width: 128,
            height: 64,
            ..cfg(2)
        };
        let mut w = new_world_with(&cfg, kinds.clone());
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
        let kinds = bare();
        let cfg = WorldConfig {
            width: 128,
            height: 64,
            ..cfg(3)
        };
        let mut w = new_world_with(&cfg, kinds.clone());
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
            hungry.needs[0] = hours(2) as i32;
            assert!(place_actor(&mut w, fox, FOX, hungry));
        }
        let mut wounded = false;
        for _ in 0..64 {
            step(&mut w);
            check_invariants(&mut w);
            wounded |= rows(&mut w)
                .iter()
                .any(|r| r.1 == CHICKEN && r.3.needs[2] == 10);
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
                f.3.needs[0] > hours(23) as i32,
                "fox {:x} ate a whole chicken: {}",
                f.0,
                f.3.needs[0]
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
               when mode == 4 and nearest free within 1 as c => { mode = 5  spawn bee at c with (7, x) }
               when true => idle }",
        )
        .unwrap();
        let (pot, stone, bee) = (0, 1, 2);
        let cfg = WorldConfig {
            width: 128,
            height: 64,
            ..cfg(9)
        };
        let mut w = new_world_with(&cfg, kinds.clone());
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

    /// Grass is ground cover: a hungry chicken walks onto a patch, stands on
    /// a tuft (both layers of one cell taken) and grazes it underfoot, a
    /// quarter tuft a bite, until the tuft is gone; the patch never blocks.
    #[test]
    fn chickens_walk_onto_grass_and_graze_it() {
        use crate::actors::systems::newborn;
        use crate::actors::{Tally, life};
        use crate::rules::{CHICKEN, GRASS};
        use crate::time::hours;
        let kinds = bare();
        let cfg = WorldConfig {
            width: 64,
            height: 64,
            ..cfg(6)
        };
        let mut w = new_world_with(&cfg, kinds.clone());
        flatten(&mut w);
        let now = tick(&w);
        for y in 20..25 {
            for x in 20..25 {
                let mut tuft = newborn(&kinds, GRASS, (x * 100 + y) as u64, now);
                tuft.needs[0] = hours(40) as i32; // no water here: keep it alive for the test
                assert!(place_actor(&mut w, Pos::new(x, y), GRASS, tuft));
            }
        }
        let mut hungry = newborn(&kinds, CHICKEN, 0xC0, now);
        hungry.needs[0] = hours(6) as i32;
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
            c.3.needs[0] > hours(7) as i32,
            "grazing fed it: {}",
            c.3.needs[0]
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
        let kinds = bare();
        let cfg = WorldConfig {
            width: 64,
            height: 64,
            ..cfg(4)
        };
        let mut w = new_world_with(&cfg, kinds.clone());
        flatten(&mut w);
        let now = tick(&w);
        let mut hungry = newborn(&kinds, CHICKEN, 0xC0, now);
        hungry.needs[0] = hours(6) as i32;
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
            grazer.3.needs[0] > (hours(12) - minutes(10)) as i32,
            "6h + two seeds of 3h: {}",
            grazer.3.needs[0]
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
        let cfg = WorldConfig {
            width: 256,
            height: 256,
            ..cfg(12)
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
        let mut w = new_world(&cfg(4));
        save(&mut w, &store).unwrap();
        let c = ChunkCoord::new(0, 0);
        let mut saved = store.read_chunk(c).unwrap().unwrap();
        saved.data.actors.rows[0].kind = 7;
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
        let mut w = bare_world(&cfg(3));
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
        let mut w = bare_world(&cfg(11));
        step(&mut w);
        step(&mut w);
        let (cc, i) = Pos::new(100, 60).split();
        stage::chunk_mut(&mut w, cc).unwrap().feature[i] = Feature::Rock;
        assert_eq!(save(&mut w, &store).unwrap(), 1);
        assert_eq!(save(&mut w, &store).unwrap(), 0);
        let expect = checksum(&mut w);

        let mut back = open_bare(&store).unwrap().unwrap();
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
        assert_eq!(open_bare(&tmp_store("empty")).unwrap().map(|_| ()), None);
        std::fs::remove_dir_all(store.dir()).unwrap();
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
