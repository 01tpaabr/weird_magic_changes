//! The whole simulation state, its tick, and chunk streaming.
//!
//! A tick is a fixed sequence of **phases**. Inside a phase, work is split
//! into independent chunks (`stage.cells.par_iter_mut()`) with no shared
//! mutable state; between phases there is a barrier. `World::step` is the
//! only place that decides phase order.
//!
//! Only loaded chunks simulate. Which chunks are loaded is a function of the
//! inputs (camera moves, [`LoadPolicy`]), so a replay with the same inputs
//! loads the same chunks in the same order and stays bit-identical.
//!
//! Time is the integer `tick` (see [`crate::time`]). A chunk that is not
//! loaded is frozen: its save file records the tick it was last simulated
//! (`last_ticked`), which is all a future catch-up-on-load needs.

use std::io;

use rayon::prelude::*;

use crate::stage::worldgen::{GenParams, generate_chunk};
use crate::stage::{CHUNK_SIZE, ChunkCoord, Pos, Stage};
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

#[derive(Debug)]
pub struct World {
    pub seed: u64,
    /// Ticks since the world began; see [`crate::time`] for the calendar.
    pub tick: u64,
    pub params: GenParams,
    pub initial_width: u32,
    pub initial_height: u32,
    pub stage: Stage,
}

impl World {
    /// Generate a fresh world with its initial region loaded.
    /// Same config => bit-identical world.
    pub fn new(cfg: &WorldConfig) -> Self {
        let mut w = Self {
            seed: cfg.seed,
            tick: START_TICK,
            params: cfg.params,
            initial_width: cfg.width,
            initial_height: cfg.height,
            stage: Stage::new(),
        };
        let cx = i32::try_from(cfg.width.div_ceil(CHUNK_SIZE as u32)).expect("width");
        let cy = i32::try_from(cfg.height.div_ceil(CHUNK_SIZE as u32)).expect("height");
        let coords: Vec<ChunkCoord> = (0..cy)
            .flat_map(|y| (0..cx).map(move |x| ChunkCoord::new(x, y)))
            .collect();
        w.load_chunks(&coords, None).expect("no store, no io");
        w
    }

    /// Open a saved world. Nothing is loaded yet: call [`World::ensure_loaded`]
    /// around the camera. `Ok(None)` if the store holds no world.
    pub fn open(store: &Store) -> io::Result<Option<Self>> {
        let Some(m) = store.read_meta()? else {
            return Ok(None);
        };
        Ok(Some(Self {
            seed: m.seed,
            tick: m.tick,
            params: m.params,
            initial_width: m.initial_width,
            initial_height: m.initial_height,
            stage: Stage::new(),
        }))
    }

    pub fn meta(&self) -> WorldMeta {
        WorldMeta {
            seed: self.seed,
            tick: self.tick,
            initial_width: self.initial_width,
            initial_height: self.initial_height,
            params: self.params,
        }
    }

    /// Write metadata and every dirty chunk; dirty flags are cleared.
    pub fn save(&mut self, store: &Store) -> io::Result<usize> {
        store.write_meta(&self.meta())?;
        let mut written = 0;
        for s in self.stage.active().to_vec() {
            let m = &mut self.stage.meta[s as usize];
            if m.dirty {
                store.write_chunk(m.coord, &self.stage.cells[s as usize], self.tick)?;
                m.dirty = false;
                m.last_ticked = self.tick;
                written += 1;
            }
        }
        Ok(written)
    }

    /// Bring the chunks around `focus` into memory and drop far ones.
    /// Load order is coordinate order; unload order likewise; both are
    /// independent of thread count. Without a store, dirty chunks are never
    /// unloaded (nothing could bring them back).
    pub fn ensure_loaded(
        &mut self,
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
        for y in fc.y - policy.load..=fc.y + policy.load {
            for x in fc.x - policy.load..=fc.x + policy.load {
                let c = ChunkCoord::new(x, y);
                if !self.stage.is_loaded(c) {
                    wanted.push(c);
                }
            }
        }
        let (g, r) = self.load_chunks(&wanted, store)?;
        stats.generated = g;
        stats.read = r;

        // Unload.
        let far: Vec<ChunkCoord> = self
            .stage
            .loaded_coords()
            .filter(|c| (c.x - fc.x).abs() > policy.unload || (c.y - fc.y).abs() > policy.unload)
            .collect();
        for c in far {
            let slot = self.stage.slot(c).expect("listed as loaded") as usize;
            let dirty = self.stage.meta[slot].dirty;
            match (dirty, store) {
                (true, Some(st)) => {
                    st.write_chunk(c, &self.stage.cells[slot], self.tick)?;
                    stats.written += 1;
                }
                (true, None) => continue,
                (false, _) => {}
            }
            self.stage.remove(c);
            stats.unloaded += 1;
        }
        Ok(stats)
    }

    /// Load `coords` (none may be loaded already): from the store when saved
    /// there, generated otherwise. Returns `(generated, read)`.
    fn load_chunks(
        &mut self,
        coords: &[ChunkCoord],
        store: Option<&Store>,
    ) -> io::Result<(usize, usize)> {
        let mut to_gen = Vec::with_capacity(coords.len());
        let mut read = 0;
        self.stage.reserve(coords.len());
        for &c in coords {
            match store.map(|s| s.read_chunk(c)).transpose()?.flatten() {
                Some(saved) => {
                    let slot = self.stage.insert(c, saved.cells, false);
                    self.stage.meta[slot as usize].last_ticked = saved.last_ticked;
                    read += 1;
                }
                None => to_gen.push(c),
            }
        }
        // Generate straight into the slab: each chunk is written exactly once.
        let mut slots: Vec<u32> = to_gen
            .iter()
            .map(|&c| self.stage.insert_blank(c, false))
            .collect();
        for &s in &slots {
            self.stage.meta[s as usize].last_ticked = self.tick;
        }
        let mut pairs: Vec<(u32, ChunkCoord)> =
            slots.iter().copied().zip(to_gen.iter().copied()).collect();
        pairs.sort_unstable_by_key(|&(s, _)| s);
        slots.clear();
        slots.extend(pairs.iter().map(|&(s, _)| s));
        let (seed, params) = (self.seed, self.params);
        self.stage
            .cells_mut_at(&slots)
            .into_par_iter()
            .zip(pairs.par_iter())
            .for_each(|(cells, &(_, c))| generate_chunk(seed, &params, c, cells));
        Ok((to_gen.len(), read))
    }

    /// Advance one tick. Phases are listed here, in order, and nowhere else.
    pub fn step(&mut self) {
        // phase 1..n: (no systems yet; the stage is static until actors land)
        self.tick += 1;
    }

    /// Checksum of all loaded state, for determinism tests and bug reports.
    pub fn checksum(&self) -> u64 {
        crate::rng::splitmix64(self.stage.checksum() ^ self.tick)
    }
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

    fn tmp_store(name: &str) -> Store {
        let dir =
            std::env::temp_dir().join(format!("wmc-world-test-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        Store::open(dir).unwrap()
    }

    #[test]
    fn new_world_covers_initial_region_in_whole_chunks() {
        let w = World::new(&cfg(1));
        assert_eq!(w.stage.loaded_count(), 3 * 2);
        assert!(w.stage.get(Pos::new(191, 127)).is_some());
        assert!(w.stage.get(Pos::new(192, 0)).is_none());
        assert!(w.stage.get(Pos::new(-1, 0)).is_none());
    }

    #[test]
    fn new_world_starts_at_dawn() {
        let w = World::new(&cfg(5));
        assert_eq!(w.tick, START_TICK);
        assert_eq!(crate::time::Clock::at(w.tick).to_string(), "day 0 06:00");
        for &s in w.stage.active() {
            assert_eq!(w.stage.meta[s as usize].last_ticked, START_TICK);
        }
    }

    #[test]
    fn step_advances_tick_and_changes_checksum() {
        let mut w = World::new(&cfg(5));
        let c0 = w.checksum();
        w.step();
        assert_eq!(w.tick, START_TICK + 1);
        assert_ne!(w.checksum(), c0);
    }

    fn checksum_after(threads: usize, ticks: u64) -> u64 {
        rayon::ThreadPoolBuilder::new()
            .num_threads(threads)
            .build()
            .unwrap()
            .install(|| {
                let mut w = World::new(&cfg(77));
                for _ in 0..ticks {
                    w.step();
                }
                w.checksum()
            })
    }

    /// The determinism gate every system must keep passing: same seed, same
    /// number of ticks, same checksum on one thread and on many.
    #[test]
    fn stepping_is_identical_across_thread_counts() {
        let one = checksum_after(1, 200);
        assert_eq!(one, checksum_after(8, 200));
        assert_eq!(one, checksum_after(3, 200));
        assert_ne!(one, checksum_after(1, 199));
    }

    #[test]
    fn streaming_loads_generates_and_unloads_clean_chunks() {
        let mut w = World::new(&cfg(9));
        let policy = LoadPolicy { load: 1, unload: 2 };
        let s = w.ensure_loaded(Pos::new(-500, -500), policy, None).unwrap();
        assert_eq!(s.generated, 9);
        assert_eq!(s.read, 0);
        assert_eq!(s.unloaded, 6); // the initial region is far away and clean
        assert_eq!(w.stage.loaded_count(), 9);
        // Second call at the same place is a no-op.
        let s = w.ensure_loaded(Pos::new(-500, -500), policy, None).unwrap();
        assert_eq!(s, StreamStats::default());
        // Regenerated chunks are identical to the originals.
        let a = World::new(&cfg(9)).stage.checksum();
        w.ensure_loaded(Pos::new(70, 30), LoadPolicy { load: 2, unload: 2 }, None)
            .unwrap();
        // Focus chunk (1,0), radius 2 => x in -1..=3, y in -2..=2, minus the far ones.
        assert!(w.stage.loaded_count() > 6);
        let mut fresh = World::new(&cfg(9));
        fresh
            .ensure_loaded(Pos::new(70, 30), LoadPolicy { load: 2, unload: 2 }, None)
            .unwrap();
        assert_eq!(w.stage.checksum(), fresh.stage.checksum());
        let _ = a;
    }

    #[test]
    fn dirty_chunks_survive_unload_only_through_a_store() {
        let mut w = World::new(&cfg(3));
        let p = Pos::new(10, 10);
        let (cc, i) = p.split();
        w.stage.chunk_mut(cc).unwrap().feature[i] = Feature::Rock;
        w.stage.chunk_mut(cc).unwrap().ground[i] = Ground::Water;
        let policy = LoadPolicy { load: 0, unload: 0 };

        // No store: the dirty chunk stays resident.
        let s = w.ensure_loaded(Pos::new(1000, 1000), policy, None).unwrap();
        assert_eq!(s.unloaded, 5);
        assert!(w.stage.is_loaded(cc));

        // With a store: written on unload, read back on load, bit-exact, and
        // stamped with the tick it was frozen at.
        let store = tmp_store("dirty");
        let before = w.stage.chunk(cc).unwrap().hash();
        w.step();
        w.step();
        let frozen_at = w.tick;
        let s = w
            .ensure_loaded(Pos::new(1000, 1000), policy, Some(&store))
            .unwrap();
        assert_eq!((s.written, s.unloaded), (1, 1));
        assert!(!w.stage.is_loaded(cc));
        w.step();
        let s = w.ensure_loaded(p, policy, Some(&store)).unwrap();
        assert_eq!((s.read, s.generated), (1, 0));
        assert_eq!(w.stage.chunk(cc).unwrap().hash(), before);
        let m = w.stage.meta[w.stage.slot(cc).unwrap() as usize];
        assert!(!m.dirty);
        assert_eq!(m.last_ticked, frozen_at);
        assert_eq!(w.tick, frozen_at + 1);
        std::fs::remove_dir_all(store.dir()).unwrap();
    }

    #[test]
    fn save_and_open_roundtrip() {
        let store = tmp_store("save");
        let mut w = World::new(&cfg(11));
        w.step();
        w.step();
        let (cc, i) = Pos::new(100, 60).split();
        w.stage.chunk_mut(cc).unwrap().feature[i] = Feature::Rock;
        assert_eq!(w.save(&store).unwrap(), 1);
        assert_eq!(w.save(&store).unwrap(), 0);
        let expect = w.checksum();

        let mut back = World::open(&store).unwrap().unwrap();
        assert_eq!((back.tick, back.seed), (START_TICK + 2, 11));
        assert_eq!(back.stage.loaded_count(), 0);
        // Load exactly the initial region again.
        back.ensure_loaded(
            Pos::new(64, 64),
            LoadPolicy { load: 1, unload: 1 },
            Some(&store),
        )
        .unwrap();
        // (0..=2, 0..=2) minus row 2, which the original never had: prune it.
        for c in back
            .stage
            .loaded_coords()
            .filter(|c| c.y == 2)
            .collect::<Vec<_>>()
        {
            back.stage.remove(c);
        }
        assert_eq!(back.checksum(), expect);
        std::fs::remove_dir_all(store.dir()).unwrap();
    }
}
