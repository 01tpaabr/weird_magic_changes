//! Save files never panic: a small valid save, then its world file or its
//! chunk file truncated, flipped, overwritten with extreme numbers, or
//! rewritten with a header no save should have. Opening it (under the
//! rules that wrote it, or ones that number its kinds otherwise), streaming
//! its chunk in, stepping, saving and reloading its rules each return `Ok`
//! or `Err`, never panic. A valid save round-trips. `make fuzz` runs many
//! more cases.

use std::sync::LazyLock;
use std::sync::atomic::{AtomicUsize, Ordering};

use bevy_ecs::world::World;
use proptest::prelude::*;
use proptest::sample::Index;

use super::*;
use crate::scenario::Scenario;
use crate::sim::{self, LoadPolicy};
use crate::stage::Pos;

/// One chunk: a drawn corner, shares, explicit starts with `with`, and
/// kinds that think every few ticks.
const SCENARIO: &str = "seed 7
size 64 64
terrain water_level 0 rock_on_soil 0 rock_on_water 0
map {
  .~#.
  .CF.
}
legend {
  . soil
  ~ water
  # rock
  C chicken
  F fox with (food = 2h, chase = 3)
}
start grass 1 / 10
start seed 1 / 50
start hive at (30, 30)
start bee at (32, 30) with (home_x = 30, home_y = 30)
";

/// The one chunk, alone: nothing else streams in.
const FOCUS: Pos = Pos::new(20, 20);
const ONE: LoadPolicy = LoadPolicy { load: 0, unload: 0 };
const COORD: ChunkCoord = ChunkCoord::new(0, 0);

/// Where a chunk file's rows start: header, coord, `last_ticked`, the
/// cell layers, the row count.
const ROWS_AT: usize = 28 + CHUNK_CELLS * (10 + SCENT_CHANNELS) + 4;
/// Where a world file's starts start: header, seed, tick, ticks/day,
/// size, terrain, the start count.
const STARTS_AT: usize = 12 + 3 * 8 + 2 * 4 + 4 * 4 + 4;

static BUILTIN: LazyLock<Kinds> = LazyLock::new(Kinds::builtin);

/// The built-in rules with their files in reverse order: the same kinds,
/// numbered otherwise, so a save opens through a remap.
static REVERSED: LazyLock<Kinds> = LazyLock::new(|| {
    let mut files = crate::rules::builtin::FILES;
    files.reverse();
    crate::rules::compile_files(&files).expect("the built-in rules compile in any order")
});

/// A fresh, empty save directory.
fn store() -> Store {
    static N: AtomicUsize = AtomicUsize::new(0);
    let n = N.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("wmc-store-fuzz-{}-{n}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    Store::open(dir).unwrap()
}

fn scenario(seed: u64) -> Scenario {
    Scenario {
        seed,
        ..Scenario::parse("s.scenario", SCENARIO).unwrap()
    }
}

/// A world of [`SCENARIO`] at `seed` after `ticks`, saved in `store`.
fn saved(store: &Store, seed: u64, ticks: u32) -> World {
    let mut w = sim::new_world_with(&scenario(seed), BUILTIN.clone()).unwrap();
    for _ in 0..ticks {
        sim::step(&mut w);
    }
    sim::save(&mut w, store).unwrap();
    w
}

/// The base save's two files.
struct Base {
    world: Vec<u8>,
    chunk: Vec<u8>,
}

static BASE: LazyLock<Base> = LazyLock::new(|| {
    let s = store();
    saved(&s, 7, 20);
    assert_eq!(s.saved_chunks().unwrap(), [COORD]);
    let base = Base {
        world: fs::read(s.meta_path()).unwrap(),
        chunk: fs::read(s.chunk_path(COORD)).unwrap(),
    };
    fs::remove_dir_all(s.dir()).unwrap();
    base
});

/// A save of these two files.
fn save_of(world: &[u8], chunk: &[u8]) -> Store {
    let s = store();
    fs::write(s.meta_path(), world).unwrap();
    fs::write(s.chunk_path(COORD), chunk).unwrap();
    s
}

/// The world `store` holds opened under `kinds`, its chunk streamed in.
fn reopen(store: &Store, kinds: &Kinds) -> World {
    let mut w = sim::open_world_with(store, kinds.clone()).unwrap().unwrap();
    sim::ensure_loaded(&mut w, FOCUS, ONE, Some(store)).unwrap();
    w
}

/// Everything that reads a save, none of which may panic: open it under
/// `kinds`, stream its chunk in, step, save (which moves a remapped save
/// over first), reload the built-in rules, stream away and back. Then
/// remove it.
fn survive(store: &Store, kinds: &Kinds) {
    let _ = store.read_meta();
    let _ = store.saved_chunks();
    let _ = store.read_chunk(COORD);
    if let Ok(Some(mut w)) = sim::open_world_with(store, kinds.clone()) {
        let _ = sim::ensure_loaded(&mut w, FOCUS, ONE, Some(store));
        for _ in 0..16 {
            sim::step(&mut w);
        }
        let _ = sim::checksum(&mut w);
        let _ = sim::save(&mut w, store);
        let _ = crate::reload::reload_rules(&mut w, Some(store), BUILTIN.clone());
        let _ = sim::ensure_loaded(&mut w, Pos::new(64 * 8, 0), ONE, Some(store));
        let _ = sim::ensure_loaded(&mut w, FOCUS, ONE, Some(store));
        sim::step(&mut w);
    }
    fs::remove_dir_all(store.dir()).unwrap();
}

/// One way to break a file.
#[derive(Debug, Clone)]
enum Corrupt {
    Truncate(Index),
    /// XOR these bytes anywhere.
    Flip(Vec<(Index, u8)>),
    /// XOR these bytes past the fixed header: a chunk's rows, a world's
    /// starts, map and kind table.
    FlipBody(Vec<(Index, u8)>),
    /// A little-endian number over the bytes there.
    U32(Index, u32),
    U64(Index, u64),
    /// Bytes gone from the middle.
    Cut(Index, Index),
    Append(Vec<u8>),
}

fn corrupt() -> impl Strategy<Value = Corrupt> {
    let flips = || prop::collection::vec((any::<Index>(), 1u8..=255), 1..8);
    let u32s = prop_oneof![
        Just(0u32),
        Just(1),
        Just(u32::MAX),
        Just(i32::MAX as u32),
        Just(i32::MIN as u32),
        Just(1 << 16),
        Just(1 << 24),
        Just(FORMAT_VERSION + 1),
        Just(2 * CHUNK_CELLS as u32 + 1),
        any::<u32>(),
    ];
    let u64s = prop_oneof![
        Just(0u64),
        Just(u64::MAX),
        Just(u64::MAX / 2),
        Just(u64::MAX / 2 + 1),
        Just(u64::from(u32::MAX) + 1),
        any::<u64>(),
    ];
    prop_oneof![
        any::<Index>().prop_map(Corrupt::Truncate),
        flips().prop_map(Corrupt::Flip),
        flips().prop_map(Corrupt::FlipBody),
        (any::<Index>(), u32s).prop_map(|(i, v)| Corrupt::U32(i, v)),
        (any::<Index>(), u64s).prop_map(|(i, v)| Corrupt::U64(i, v)),
        (any::<Index>(), any::<Index>()).prop_map(|(a, b)| Corrupt::Cut(a, b)),
        prop::collection::vec(any::<u8>(), 1..16).prop_map(Corrupt::Append),
    ]
}

/// `bytes` broken by `c`; `body` is where the fixed header ends.
fn apply(bytes: &[u8], c: &Corrupt, body: usize) -> Vec<u8> {
    let mut b = bytes.to_vec();
    let n = b.len();
    match c {
        Corrupt::Truncate(i) => b.truncate(i.index(n)),
        Corrupt::Flip(f) => {
            for (i, x) in f {
                b[i.index(n)] ^= x;
            }
        }
        Corrupt::FlipBody(f) => {
            for (i, x) in f {
                b[body + i.index(n - body)] ^= x;
            }
        }
        Corrupt::U32(i, v) => {
            let at = i.index(n - 3);
            b[at..at + 4].copy_from_slice(&v.to_le_bytes());
        }
        Corrupt::U64(i, v) => {
            let at = i.index(n - 7);
            b[at..at + 8].copy_from_slice(&v.to_le_bytes());
        }
        Corrupt::Cut(i, j) => {
            let from = i.index(n);
            let to = from + j.index(n - from + 1).min(64);
            b.drain(from..to);
        }
        Corrupt::Append(x) => b.extend_from_slice(x),
    }
    b
}

proptest! {
    #![proptest_config(crate::fuzz_config(48))]

    /// A broken world or chunk file is refused or read, never a panic.
    #[test]
    fn a_corrupt_save_never_panics(
        chunk in any::<bool>(),
        c in corrupt(),
        remapped in any::<bool>(),
    ) {
        let base = &*BASE;
        let s = if chunk {
            save_of(&base.world, &apply(&base.chunk, &c, ROWS_AT))
        } else {
            save_of(&apply(&base.world, &c, STARTS_AT), &base.chunk)
        };
        survive(&s, if remapped { &REVERSED } else { &BUILTIN });
    }
}

proptest! {
    #![proptest_config(crate::fuzz_config(8))]

    /// A saved world reopens to the same checksum, under its own rules or
    /// through a remap and back, and runs on identically.
    #[test]
    fn a_save_round_trips(seed in any::<u64>(), ticks in 0u32..40) {
        let s = store();
        let mut w = saved(&s, seed, ticks);
        let want = sim::checksum(&mut w);
        let mut back = reopen(&s, &BUILTIN);
        prop_assert_eq!(sim::checksum(&mut back), want);
        for _ in 0..8 {
            sim::step(&mut w);
            sim::step(&mut back);
        }
        prop_assert_eq!(sim::checksum(&mut back), sim::checksum(&mut w));
        // Through the other numbering: saving moves the files over, and
        // opening them under the first rules moves them back.
        let mut other = reopen(&s, &REVERSED);
        sim::save(&mut other, &s).unwrap();
        let mut again = reopen(&s, &BUILTIN);
        prop_assert_eq!(sim::checksum(&mut again), want);
        fs::remove_dir_all(s.dir()).unwrap();
    }
}

/// Every truncation of the world file is refused; of the chunk file, every
/// length up to its first cells, each section boundary and a sample of the
/// rest.
#[test]
fn every_truncated_save_is_refused() {
    let base = &*BASE;
    let s = store();
    for len in 0..base.world.len() {
        fs::write(s.meta_path(), &base.world[..len]).unwrap();
        assert!(s.read_meta().is_err(), "world file cut at {len}");
    }
    let mut cuts: Vec<usize> = (0..64).collect();
    for at in [
        28,
        28 + CHUNK_CELLS,
        28 + 2 * CHUNK_CELLS,
        ROWS_AT - 4,
        ROWS_AT,
    ] {
        cuts.extend([at - 1, at, at + 1]);
    }
    cuts.extend((0..base.chunk.len()).step_by(997));
    cuts.extend(base.chunk.len() - 100..base.chunk.len());
    for len in cuts {
        assert!(
            Store::decode_chunk(COORD, &base.chunk[..len]).is_err(),
            "chunk file cut at {len}"
        );
    }
    fs::remove_dir_all(s.dir()).unwrap();
}

/// World headers no save should have: the wrong version, huge counts, a
/// share over zero, a tick at the end of time, a size past any world. Each
/// is refused, or opens and runs; none panics.
#[test]
fn a_bad_world_header_never_panics() {
    let base = &*BASE;
    let s = save_of(&base.world, &base.chunk);
    let meta = s.read_meta().unwrap().unwrap();
    fs::remove_dir_all(s.dir()).unwrap();
    let share = |num, den| Start::Share {
        kind: "grass".into(),
        num,
        den,
    };
    let at = |x, y| Start::at("hive", x, y);
    // (what, the meta, whether opening refuses it)
    let cases: Vec<(&str, WorldMeta, bool)> = vec![
        (
            "den 0",
            WorldMeta {
                starts: vec![share(1, 0)],
                ..meta.clone()
            },
            true,
        ),
        (
            "num 0",
            WorldMeta {
                starts: vec![share(0, 5)],
                ..meta.clone()
            },
            true,
        ),
        (
            "num > den",
            WorldMeta {
                starts: vec![share(9, 5)],
                ..meta.clone()
            },
            true,
        ),
        (
            "shares over one",
            WorldMeta {
                starts: vec![
                    share(3, 4),
                    Start::Share {
                        kind: "seed".into(),
                        num: 1,
                        den: 2,
                    },
                ],
                ..meta.clone()
            },
            true,
        ),
        (
            "a share of all",
            WorldMeta {
                starts: vec![share(1, 1)],
                ..meta.clone()
            },
            false,
        ),
        (
            "a tiny share",
            WorldMeta {
                starts: vec![share(1, u32::MAX)],
                ..meta.clone()
            },
            false,
        ),
        (
            "two starts on a cell",
            WorldMeta {
                starts: vec![at(5, 5), at(5, 5)],
                ..meta.clone()
            },
            true,
        ),
        (
            "a start on rock",
            WorldMeta {
                starts: vec![at(2, 0)],
                ..meta.clone()
            },
            true,
        ),
        (
            "starts at the ends of i32",
            WorldMeta {
                starts: vec![
                    at(i32::MIN, i32::MIN),
                    at(i32::MAX, i32::MAX),
                    at(i32::MIN, i32::MAX),
                ],
                ..meta.clone()
            },
            true,
        ),
        (
            "starts at the ends of the world",
            WorldMeta {
                starts: vec![
                    at(-WORLD_EXTENT, -WORLD_EXTENT),
                    at(WORLD_EXTENT - 1, WORLD_EXTENT - 1),
                    at(-WORLD_EXTENT, WORLD_EXTENT - 1),
                ],
                ..meta.clone()
            },
            false,
        ),
        (
            "a start of a kind the rules lack",
            WorldMeta {
                starts: vec![
                    at(5, 5),
                    Start::at("dragon", 6, 6),
                    Start::Share {
                        kind: "wyrm".into(),
                        num: 1,
                        den: 3,
                    },
                ],
                ..meta.clone()
            },
            false,
        ),
        (
            "a need past its max, a mem the kind lacks",
            WorldMeta {
                starts: vec![Start::At {
                    kind: "hive".into(),
                    x: 5,
                    y: 5,
                    with: vec![
                        ("nectar".into(), i32::MAX),
                        ("health".into(), i32::MIN),
                        ("nope".into(), 1),
                    ],
                }],
                ..meta.clone()
            },
            false,
        ),
        (
            "the last tick",
            WorldMeta {
                tick: u64::MAX / 2,
                ..meta.clone()
            },
            false,
        ),
        (
            "past the last tick",
            WorldMeta {
                tick: u64::MAX / 2 + 1,
                ..meta.clone()
            },
            true,
        ),
        (
            "tick 0",
            WorldMeta {
                tick: 0,
                ..meta.clone()
            },
            false,
        ),
        (
            "a size past the world",
            WorldMeta {
                initial_width: u32::MAX,
                initial_height: u32::MAX,
                ..meta.clone()
            },
            true,
        ),
        (
            "the largest size",
            WorldMeta {
                initial_width: WORLD_EXTENT.unsigned_abs(),
                initial_height: WORLD_EXTENT.unsigned_abs(),
                ..meta.clone()
            },
            false,
        ),
        (
            "size 0",
            WorldMeta {
                initial_width: 0,
                initial_height: 0,
                ..meta.clone()
            },
            false,
        ),
        (
            "NaN terrain",
            WorldMeta {
                params: crate::stage::worldgen::GenParams {
                    water_level: f32::NAN,
                    ..meta.params
                },
                ..meta.clone()
            },
            true,
        ),
        (
            "a water scale of zero",
            WorldMeta {
                params: crate::stage::worldgen::GenParams {
                    water_scale: 0.0,
                    ..meta.params
                },
                ..meta.clone()
            },
            true,
        ),
        (
            "a water scale near zero",
            WorldMeta {
                params: crate::stage::worldgen::GenParams {
                    water_scale: f32::MIN_POSITIVE,
                    ..meta.params
                },
                ..meta.clone()
            },
            false,
        ),
        (
            "a map byte that is no cell",
            WorldMeta {
                map: Some(DrawnMap {
                    width: 1,
                    height: 1,
                    cells: vec![0xFF],
                    outside: None,
                }),
                ..meta.clone()
            },
            true,
        ),
        (
            "an outside that is no cell",
            WorldMeta {
                map: Some(DrawnMap {
                    width: 1,
                    height: 1,
                    cells: vec![0],
                    outside: Some(0x22),
                }),
                ..meta.clone()
            },
            true,
        ),
        (
            "an empty map",
            WorldMeta {
                map: Some(DrawnMap {
                    width: 0,
                    height: 0,
                    cells: vec![],
                    outside: Some(0),
                }),
                ..meta.clone()
            },
            false,
        ),
        (
            "no kinds",
            WorldMeta {
                kinds: vec![],
                ..meta.clone()
            },
            false,
        ),
        (
            "a kind the rules lack",
            WorldMeta {
                kinds: [
                    meta.kinds.clone(),
                    vec![SavedKind {
                        name: "dragon".into(),
                        cover: false,
                        needs: vec![],
                        mems: vec![],
                        states: vec![],
                    }],
                ]
                .concat(),
                ..meta.clone()
            },
            true,
        ),
        (
            "a kind twice",
            WorldMeta {
                kinds: [meta.kinds.clone(), meta.kinds[..1].to_vec()].concat(),
                ..meta.clone()
            },
            false,
        ),
        (
            "a kind that became ground cover",
            WorldMeta {
                kinds: meta
                    .kinds
                    .iter()
                    .map(|k| SavedKind {
                        cover: !k.cover,
                        ..k.clone()
                    })
                    .collect(),
                ..meta.clone()
            },
            true,
        ),
        (
            "slot names shuffled and doubled",
            WorldMeta {
                kinds: meta
                    .kinds
                    .iter()
                    .map(|k| SavedKind {
                        needs: k.needs.iter().rev().cloned().collect(),
                        mems: k.mems.iter().map(|_| "same".to_string()).collect(),
                        states: vec!["x".into(); 300],
                        ..k.clone()
                    })
                    .collect(),
                ..meta.clone()
            },
            false,
        ),
        (
            "too many needs",
            WorldMeta {
                kinds: meta
                    .kinds
                    .iter()
                    .map(|k| SavedKind {
                        needs: vec!["n".into(); NEED_SLOTS + 1],
                        ..k.clone()
                    })
                    .collect(),
                ..meta.clone()
            },
            true,
        ),
        (
            "scent channels renamed",
            WorldMeta {
                scents: vec!["x".into(), "y".into()],
                ..meta.clone()
            },
            false,
        ),
        (
            "too many scent channels",
            WorldMeta {
                scents: vec!["x".into(); SCENT_CHANNELS + 1],
                ..meta.clone()
            },
            true,
        ),
        (
            "packs that do not exist",
            WorldMeta {
                packs: vec!["/nowhere".into(), String::new()],
                ..meta.clone()
            },
            false,
        ),
    ];
    for (what, m, refused) in cases {
        for kinds in [&*BUILTIN, &*REVERSED] {
            let s = store();
            s.write_meta(&m).unwrap();
            fs::write(s.chunk_path(COORD), &base.chunk).unwrap();
            let opened = sim::open_world_with(&s, kinds.clone());
            assert_eq!(opened.is_err(), refused, "{what}: {:?}", opened.err());
            survive(&s, kinds);
        }
    }
    // A header this build does not read: another version, another chunk
    // size, another day, a count past what the file holds.
    for (at, v) in [
        (4, FORMAT_VERSION + 1),
        (4, 0),
        (8, CHUNK_BITS + 1),
        (28, 1),
        (STARTS_AT - 4, u32::MAX),
        (STARTS_AT - 4, 1 << 24),
    ] {
        let mut bytes = base.world.clone();
        bytes[at..at + 4].copy_from_slice(&v.to_le_bytes());
        let s = save_of(&bytes, &base.chunk);
        assert!(s.read_meta().is_err(), "{v} at {at}");
        survive(&s, &BUILTIN);
    }
}
