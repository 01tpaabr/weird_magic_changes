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

use crate::actors::{ChunkActors, ChunkMinds, systems};
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
    /// Own-chunk resolution: claims, die/become/spawn, result codes.
    Apply,
    /// Dead rows removed.
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
    world.init_resource::<Stage>();
    world.init_resource::<Tick>();
    world.insert_resource(Kinds::builtin());
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
            Phase::Apply,
            Phase::Compact,
            Phase::Advance,
        )
            .chain(),
    );
    schedule.add_systems((
        systems::think.in_set(Phase::Think),
        systems::apply.in_set(Phase::Apply),
        systems::compact.in_set(Phase::Compact),
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
    crate::par::init_task_pool();
    let mut world = World::new();
    install(&mut world);
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
    crate::par::init_task_pool();
    let mut world = World::new();
    install(&mut world);
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
    // Every chunk is a pure function of (seed, coord): generated in parallel,
    // spawned in coordinate order. Its rows are born now, needs full.
    let mut chunks: Vec<ChunkData> = generate_many(seed, &params, &to_gen);
    {
        let kinds = world.resource::<Kinds>();
        for data in &mut chunks {
            for (p, m) in data.actors.rows.iter().zip(&mut data.minds.rows) {
                *m = systems::newborn(kinds, p.kind, m.uid, now);
            }
        }
    }
    for (c, data) in to_gen.iter().zip(chunks) {
        let inhabited = !data.actors.rows.is_empty();
        stage::insert(world, *c, data, inhabited, now);
    }
    Ok((to_gen.len(), read))
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

    /// A stage with nobody on it: the streaming and save tests below count
    /// clean chunks, and an inhabited chunk is dirty by design.
    fn cfg(seed: u64) -> WorldConfig {
        WorldConfig {
            seed,
            width: 150,
            height: 70,
            params: GenParams {
                seed_density: 0.0,
                ..GenParams::default()
            },
        }
    }

    /// The default density: seeds everywhere.
    fn cfg_seeded(seed: u64) -> WorldConfig {
        WorldConfig {
            params: GenParams::default(),
            ..cfg(seed)
        }
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
        let mut w = new_world(&cfg_seeded(21));
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
        use crate::rules::{SEED, TREE};
        let mut w = new_world(&WorldConfig {
            width: 128,
            height: 128,
            ..cfg_seeded(31)
        });
        let start = count_kinds(&mut w);
        assert!(start[usize::from(SEED)] > 0 && start[usize::from(TREE)] == 0);
        for _ in 0..crate::time::days(4) {
            step(&mut w);
        }
        let end = count_kinds(&mut w);
        assert!(
            end[usize::from(TREE)] > 0,
            "seeds by water became trees: {end:?}"
        );
        assert!(
            end[usize::from(SEED)] + end[usize::from(TREE)] != start[usize::from(SEED)],
            "seeds died away from water and trees dropped new ones: {start:?} -> {end:?}"
        );
        let kinds = w.resource::<Kinds>().len();
        for (cells, a, m) in w
            .query::<(&ChunkCells, &ChunkActors, &ChunkMinds)>()
            .iter(&w)
        {
            crate::actors::validate(&cells.occupant, &a.rows, &m.rows, kinds).unwrap();
            for r in &a.rows {
                assert!(cells.walkable(usize::from(r.cell)));
            }
            for mind in &m.rows {
                assert!(mind.needs[0] > 0, "a living actor has water");
            }
        }
        // Reproducible from scratch after thousands of ticks.
        let mut again = new_world(&WorldConfig {
            width: 128,
            height: 128,
            ..cfg_seeded(31)
        });
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
        let mut w = new_world(&cfg_seeded(8));
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

    #[test]
    fn corrupt_rows_are_refused_on_load() {
        let store = tmp_store("rows");
        let mut w = new_world(&cfg_seeded(4));
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
