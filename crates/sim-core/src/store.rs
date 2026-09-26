//! Persistence: a save directory holding world metadata plus one file per
//! **modified** chunk. Clean chunks are regenerated from the seed, so a save
//! of an unexplored world is a few dozen bytes.
//!
//! ```text
//! <dir>/world.wmc              magic, version, seed, tick, ticks/day, initial size, gen params,
//!                              starts, drawn map, kinds (names, cover, slot names), scent
//!                              channels, packs, rules hash
//! <dir>/chunks/<x>_<y>.wmcc    magic, version, coord, last_ticked, each cell layer as raw bytes
//!                              (ground, feature, occupant, cover, scent channels),
//!                              then n, n public actor rows, n private actor rows, as raw bytes
//! ```
//!
//! Everything is little-endian, fixed layout, written to a temp file and
//! renamed into place (a crash mid-write leaves the old file intact). No
//! serde: the layout is `ChunkCells` and the actor rows verbatim, so a save
//! is a memcpy per array. Bump [`FORMAT_VERSION`] whenever a layer, a row
//! type, `CHUNK_BITS`, `GenParams` or a header field changes; old saves are
//! refused rather than misread. The header also carries `TICKS_PER_DAY`:
//! every duration in a world is in ticks, so a build with a different day
//! length must not open it. Kinds travel by name, with the names of their
//! need, mem and state slots and the scent channel names, so rows can be
//! checked against (and remapped to) the loaded kind table on open
//! (`sim::open`). The scenario's starts travel by kind name too.
//!
//! Revisit when a save directory grows past a few thousand chunk files:
//! pack chunks into region files (32x32 chunks per file with an offset table).

use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use bevy_ecs::resource::Resource;

use crate::actors::{ActorMind, ActorPub, ChunkActors, ChunkMinds};
use crate::rules::Kinds;
use crate::scenario::{DrawnMap, Start};
use crate::stage::worldgen::GenParams;
use crate::stage::{CHUNK_BITS, CHUNK_CELLS, ChunkCells, ChunkCoord, ChunkData};
use crate::time::TICKS_PER_DAY;

pub const FORMAT_VERSION: u32 = 9;
const WORLD_MAGIC: &[u8; 4] = b"WMCW";
const CHUNK_MAGIC: &[u8; 4] = b"WMCC";

/// World-level facts that must survive a restart: the scenario the world
/// was made from, the tick, and the shape of the rules it was saved with.
#[derive(Debug, Clone, PartialEq)]
pub struct WorldMeta {
    pub seed: u64,
    pub tick: u64,
    /// Size of the initially generated region, in cells, at `[0, w) x [0, h)`.
    pub initial_width: u32,
    pub initial_height: u32,
    pub params: GenParams,
    /// Where kinds start, by name: every clean chunk regenerates from them.
    pub starts: Vec<Start>,
    /// A drawn map's terrain (step 8e); `None`: noise everywhere.
    pub map: Option<DrawnMap>,
    /// Kind table of the rules that wrote the save, by index.
    pub kinds: Vec<SavedKind>,
    /// Scent channel names of those rules, in channel order.
    pub scents: Vec<String>,
    /// The rule packs the world is played with, as given (step 8c).
    pub packs: Vec<String>,
    /// `Kinds::hash` of the rules the save was last played with. Recorded,
    /// not enforced: rules may be tuned between sessions; the checksum
    /// says when they were.
    pub rules_hash: u64,
}

/// One kind of the rules a save was written with: what its rows' slots
/// mean, by name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SavedKind {
    pub name: String,
    pub cover: bool,
    pub needs: Vec<String>,
    pub mems: Vec<String>,
    pub states: Vec<String>,
}

impl SavedKind {
    /// A kind table as a save keeps it.
    pub fn table(kinds: &Kinds) -> Vec<SavedKind> {
        kinds
            .defs
            .iter()
            .map(|d| SavedKind {
                name: d.name.clone(),
                cover: d.cover,
                needs: d.needs.iter().map(|n| n.name.clone()).collect(),
                mems: d.mems.clone(),
                states: kinds
                    .debug
                    .states
                    .get(usize::from(d.id))
                    .cloned()
                    .unwrap_or_default(),
            })
            .collect()
    }
}

/// One chunk as it sits on disk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SavedChunk {
    pub data: ChunkData,
    /// World tick the cells correspond to: the tick at which the chunk was
    /// last simulated (i.e. written). A future catch-up on load reads
    /// `world.tick - last_ticked`; today it is only recorded.
    pub last_ticked: u64,
}

/// A save directory. Cheap to clone; holds no open files. A `Resource` so
/// the app can keep the open save beside the world it belongs to.
#[derive(Resource, Debug, Clone)]
pub struct Store {
    dir: PathBuf,
}

impl Store {
    /// Open (creating if needed) a save directory.
    pub fn open(dir: impl Into<PathBuf>) -> io::Result<Self> {
        let dir = dir.into();
        fs::create_dir_all(dir.join("chunks"))?;
        Ok(Self { dir })
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    fn meta_path(&self) -> PathBuf {
        self.dir.join("world.wmc")
    }

    /// Every chunk with a file in this store, in coordinate order (`y`, then
    /// `x`). Files that are not `<x>_<y>.wmcc` are ignored.
    pub fn saved_chunks(&self) -> io::Result<Vec<ChunkCoord>> {
        let mut out = Vec::new();
        for entry in fs::read_dir(self.dir.join("chunks"))? {
            let name = entry?.file_name();
            let Some(stem) = name.to_str().and_then(|n| n.strip_suffix(".wmcc")) else {
                continue;
            };
            if let Some((x, y)) = stem.split_once('_')
                && let (Ok(x), Ok(y)) = (x.parse(), y.parse())
            {
                out.push(ChunkCoord::new(x, y));
            }
        }
        out.sort_unstable_by_key(|c| (c.y, c.x));
        Ok(out)
    }

    fn chunk_path(&self, c: ChunkCoord) -> PathBuf {
        self.dir
            .join("chunks")
            .join(format!("{}_{}.wmcc", c.x, c.y))
    }

    /// `Ok(None)` if this directory has no world yet.
    pub fn read_meta(&self) -> io::Result<Option<WorldMeta>> {
        let bytes = match fs::read(self.meta_path()) {
            Ok(b) => b,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e),
        };
        let mut r = Reader::new(&bytes, WORLD_MAGIC)?;
        let seed = r.u64()?;
        let tick = r.u64()?;
        let ticks_per_day = r.u64()?;
        if ticks_per_day != TICKS_PER_DAY {
            return Err(bad(format!(
                "save uses {ticks_per_day} ticks/day, build uses {TICKS_PER_DAY}"
            )));
        }
        let (initial_width, initial_height) = (r.u32()?, r.u32()?);
        let params = GenParams {
            water_scale: r.f32()?,
            water_level: r.f32()?,
            rock_on_soil: r.f32()?,
            rock_on_water: r.f32()?,
        };
        let n = r.count(1 << 24, "starts")?;
        let mut starts = Vec::with_capacity(n.min(1 << 16));
        for _ in 0..n {
            let tag = r.u8()?;
            let kind = r.str()?;
            starts.push(match tag {
                0 => Start::Share {
                    kind,
                    num: r.u32()?,
                    den: r.u32()?,
                },
                // 2: an explicit start with `with` values (still format 9:
                // saves without them read as before).
                1 | 2 => {
                    let (x, y) = (r.i32()?, r.i32()?);
                    let mut with = Vec::new();
                    if tag == 2 {
                        for _ in 0..r.count(u32::from(u8::MAX), "values in a start")? {
                            with.push((r.str()?, r.i32()?));
                        }
                    }
                    Start::At { kind, x, y, with }
                }
                t => return Err(bad(format!("start tag {t}"))),
            });
        }
        let map = match r.u8()? {
            0 => None,
            1 => {
                let (width, height) = (r.u32()?, r.u32()?);
                let cells = r
                    .bytes(width as usize * height as usize)
                    .map_err(|_| bad("truncated drawn map"))?
                    .to_vec();
                let outside = match r.u8()? {
                    0 => None,
                    _ => Some(r.u8()?),
                };
                let map = DrawnMap {
                    width,
                    height,
                    cells,
                    outside,
                };
                map.validate().map_err(bad)?;
                Some(map)
            }
            t => return Err(bad(format!("map tag {t}"))),
        };
        let n = r.count(u32::from(u16::MAX), "kinds")?;
        let mut kinds = Vec::with_capacity(n);
        for _ in 0..n {
            kinds.push(SavedKind {
                name: r.str()?,
                cover: r.u8()? != 0,
                needs: r.strs()?,
                mems: r.strs()?,
                states: r.strs()?,
            });
        }
        let scents = r.strs()?;
        let packs = r.strs()?;
        let rules_hash = r.u64()?;
        r.finish()?;
        Ok(Some(WorldMeta {
            seed,
            tick,
            initial_width,
            initial_height,
            params,
            starts,
            map,
            kinds,
            scents,
            packs,
            rules_hash,
        }))
    }

    pub fn write_meta(&self, m: &WorldMeta) -> io::Result<()> {
        let mut w = Writer::new(WORLD_MAGIC);
        w.u64(m.seed);
        w.u64(m.tick);
        w.u64(TICKS_PER_DAY);
        w.u32(m.initial_width);
        w.u32(m.initial_height);
        w.f32(m.params.water_scale);
        w.f32(m.params.water_level);
        w.f32(m.params.rock_on_soil);
        w.f32(m.params.rock_on_water);
        w.len(m.starts.len());
        for s in &m.starts {
            match s {
                Start::Share { kind, num, den } => {
                    w.u8(0);
                    w.str(kind);
                    w.u32(*num);
                    w.u32(*den);
                }
                Start::At { kind, x, y, with } => {
                    w.u8(if with.is_empty() { 1 } else { 2 });
                    w.str(kind);
                    w.i32(*x);
                    w.i32(*y);
                    if !with.is_empty() {
                        w.len(with.len());
                        for (name, v) in with {
                            w.str(name);
                            w.i32(*v);
                        }
                    }
                }
            }
        }
        match &m.map {
            None => w.u8(0),
            Some(map) => {
                assert_eq!(
                    map.cells.len(),
                    map.width as usize * map.height as usize,
                    "a drawn map has a byte per cell"
                );
                w.u8(1);
                w.u32(map.width);
                w.u32(map.height);
                w.buf.extend_from_slice(&map.cells);
                match map.outside {
                    None => w.u8(0),
                    Some(b) => {
                        w.u8(1);
                        w.u8(b);
                    }
                }
            }
        }
        w.len(m.kinds.len());
        for k in &m.kinds {
            w.str(&k.name);
            w.u8(u8::from(k.cover));
            w.strs(&k.needs);
            w.strs(&k.mems);
            w.strs(&k.states);
        }
        w.strs(&m.scents);
        w.strs(&m.packs);
        w.u64(m.rules_hash);
        write_atomic(&self.meta_path(), &w.buf)
    }

    pub fn has_chunk(&self, c: ChunkCoord) -> bool {
        self.chunk_path(c).is_file()
    }

    /// `Ok(None)` if the chunk was never saved (regenerate it). Row *shape*
    /// is checked here (counts, sizes); the invariants that need the kind
    /// table (`ChunkData::validate`) are the caller's.
    pub fn read_chunk(&self, c: ChunkCoord) -> io::Result<Option<SavedChunk>> {
        let bytes = match fs::read(self.chunk_path(c)) {
            Ok(b) => b,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e),
        };
        let mut r = Reader::new(&bytes, CHUNK_MAGIC)?;
        let (x, y) = (r.i32()?, r.i32()?);
        if (x, y) != (c.x, c.y) {
            return Err(bad(format!("chunk file for {c:?} claims ({x}, {y})")));
        }
        let last_ticked = r.u64()?;
        let mut cells = ChunkCells::default();
        let ground = r.bytes(CHUNK_CELLS)?;
        let feature = r.bytes(CHUNK_CELLS)?;
        let occupant = r.bytes(CHUNK_CELLS * 4)?;
        let cover = r.bytes(CHUNK_CELLS * 4)?;
        for ch in &mut cells.scent {
            ch.copy_from_slice(r.bytes(CHUNK_CELLS)?);
        }
        cells.ground.copy_from_slice(
            bytemuck::checked::try_cast_slice(ground).map_err(|e| bad(e.to_string()))?,
        );
        cells.feature.copy_from_slice(
            bytemuck::checked::try_cast_slice(feature).map_err(|e| bad(e.to_string()))?,
        );
        // Occupant ids are u32 LE; on LE targets this is a memcpy.
        for (o, b) in cells.occupant.iter_mut().zip(occupant.as_chunks::<4>().0) {
            o.0 = u32::from_le_bytes(*b);
        }
        for (o, b) in cells.cover.iter_mut().zip(cover.as_chunks::<4>().0) {
            o.0 = u32::from_le_bytes(*b);
        }
        let n = r.u32()? as usize;
        if n > 2 * CHUNK_CELLS {
            return Err(bad(format!("{n} actor rows for {CHUNK_CELLS} cells")));
        }
        let actors = ChunkActors {
            rows: r.rows::<ActorPub>(n)?,
        };
        let minds = ChunkMinds {
            rows: r.rows::<ActorMind>(n)?,
        };
        r.finish()?;
        Ok(Some(SavedChunk {
            data: ChunkData {
                cells,
                actors,
                minds,
            },
            last_ticked,
        }))
    }

    /// Write a chunk whose state is current as of `last_ticked`.
    pub fn write_chunk(
        &self,
        c: ChunkCoord,
        cells: &ChunkCells,
        actors: &ChunkActors,
        minds: &ChunkMinds,
        last_ticked: u64,
    ) -> io::Result<()> {
        assert_eq!(
            actors.rows.len(),
            minds.rows.len(),
            "row arrays out of step"
        );
        let mut w = Writer::new(CHUNK_MAGIC);
        w.i32(c.x);
        w.i32(c.y);
        w.u64(last_ticked);
        w.buf.extend_from_slice(bytemuck::cast_slice(&cells.ground));
        w.buf
            .extend_from_slice(bytemuck::cast_slice(&cells.feature));
        for o in cells.occupant.iter().chain(&cells.cover) {
            w.buf.extend_from_slice(&o.0.to_le_bytes());
        }
        for ch in &cells.scent {
            w.buf.extend_from_slice(ch);
        }
        w.u32(u32::try_from(actors.rows.len()).expect("row count fits u32"));
        // Rows are `#[repr(C)]` Pod with explicit padding, LE on every target
        // this runs on: their bytes are the format.
        w.buf.extend_from_slice(bytemuck::cast_slice(&actors.rows));
        w.buf.extend_from_slice(bytemuck::cast_slice(&minds.rows));
        write_atomic(&self.chunk_path(c), &w.buf)
    }
}

fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let tmp = path.with_extension("tmp");
    {
        let mut f = fs::File::create(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
    }
    fs::rename(&tmp, path)
}

fn bad(msg: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, msg.into())
}

struct Writer {
    buf: Vec<u8>,
}

impl Writer {
    fn new(magic: &[u8; 4]) -> Self {
        let mut w = Self {
            buf: Vec::with_capacity(64),
        };
        w.buf.extend_from_slice(magic);
        w.u32(FORMAT_VERSION);
        // Layout fingerprint: refuses saves from a different chunk size.
        w.u32(CHUNK_BITS);
        w
    }
    fn u8(&mut self, v: u8) {
        self.buf.push(v);
    }
    fn u32(&mut self, v: u32) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }
    fn len(&mut self, n: usize) {
        self.u32(u32::try_from(n).expect("a length fits u32"));
    }
    fn str(&mut self, s: &str) {
        self.len(s.len());
        self.buf.extend_from_slice(s.as_bytes());
    }
    fn strs(&mut self, v: &[String]) {
        self.len(v.len());
        for s in v {
            self.str(s);
        }
    }
    fn i32(&mut self, v: i32) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }
    fn u64(&mut self, v: u64) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }
    fn f32(&mut self, v: f32) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }
}

struct Reader<'a> {
    rest: &'a [u8],
}

impl<'a> Reader<'a> {
    fn new(bytes: &'a [u8], magic: &[u8; 4]) -> io::Result<Self> {
        let mut r = Self { rest: bytes };
        if r.bytes(4)? != magic {
            return Err(bad("not a wmc save file"));
        }
        let version = r.u32()?;
        if version != FORMAT_VERSION {
            return Err(bad(format!(
                "save format {version}, this build reads {FORMAT_VERSION}"
            )));
        }
        let bits = r.u32()?;
        if bits != CHUNK_BITS {
            return Err(bad(format!(
                "save uses CHUNK_BITS={bits}, build uses {CHUNK_BITS}"
            )));
        }
        Ok(r)
    }
    fn bytes(&mut self, n: usize) -> io::Result<&'a [u8]> {
        if self.rest.len() < n {
            return Err(bad("truncated save file"));
        }
        let (head, tail) = self.rest.split_at(n);
        self.rest = tail;
        Ok(head)
    }
    fn u8(&mut self) -> io::Result<u8> {
        Ok(self.bytes(1)?[0])
    }
    fn u32(&mut self) -> io::Result<u32> {
        let b = self.bytes(4)?;
        Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }
    /// A count of at most `max` things.
    fn count(&mut self, max: u32, what: &str) -> io::Result<usize> {
        let n = self.u32()?;
        if n > max {
            return Err(bad(format!("{n} {what}")));
        }
        Ok(n as usize)
    }
    fn str(&mut self) -> io::Result<String> {
        let n = self.count(1 << 16, "bytes in a name")?;
        String::from_utf8(self.bytes(n)?.to_vec()).map_err(|e| bad(e.to_string()))
    }
    fn strs(&mut self) -> io::Result<Vec<String>> {
        let n = self.count(u32::from(u16::MAX), "names")?;
        (0..n).map(|_| self.str()).collect()
    }
    fn i32(&mut self) -> io::Result<i32> {
        let b = self.bytes(4)?;
        Ok(i32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }
    fn u64(&mut self) -> io::Result<u64> {
        let b = self.bytes(8)?;
        let mut a = [0u8; 8];
        a.copy_from_slice(b);
        Ok(u64::from_le_bytes(a))
    }
    fn f32(&mut self) -> io::Result<f32> {
        Ok(f32::from_bits(self.u32()?))
    }
    /// `n` Pod rows. A file buffer is not aligned for `T`, so each row is
    /// read with an unaligned copy (a memcpy of `size_of::<T>()`).
    fn rows<T: bytemuck::Pod>(&mut self, n: usize) -> io::Result<Vec<T>> {
        let size = size_of::<T>();
        let bytes = self.bytes(n * size)?;
        Ok((0..n)
            .map(|i| bytemuck::pod_read_unaligned(&bytes[i * size..(i + 1) * size]))
            .collect())
    }
    fn finish(self) -> io::Result<()> {
        if self.rest.is_empty() {
            Ok(())
        } else {
            Err(bad("trailing bytes in save file"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rules::SEED;
    use crate::scenario::{Placement, Scenario};
    use crate::stage::worldgen::generate_chunk;
    use crate::stage::{ActorId, Feature, Ground};

    /// The built-in scenario's shares against the built-in rules.
    fn builtin() -> Placement {
        let s = Scenario::builtin();
        Placement::resolve(&s.starts, &Kinds::builtin(), &s.terrain()).unwrap()
    }

    fn tmp_store(name: &str) -> Store {
        let dir =
            std::env::temp_dir().join(format!("wmc-store-test-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        Store::open(dir).unwrap()
    }

    #[test]
    fn meta_roundtrip() {
        let s = tmp_store("meta");
        assert_eq!(s.read_meta().unwrap(), None);
        let names = |v: &[&str]| v.iter().map(|n| n.to_string()).collect::<Vec<_>>();
        let m = WorldMeta {
            seed: 0xDEAD_BEEF,
            tick: 12,
            initial_width: 300,
            initial_height: 200,
            params: GenParams {
                water_scale: 9.5,
                ..GenParams::default()
            },
            starts: vec![
                Start::Share {
                    kind: "seed".into(),
                    num: 1,
                    den: 100,
                },
                Start::at("árvore", -3, 70000),
                Start::At {
                    kind: "fox".into(),
                    x: 5,
                    y: 6,
                    with: vec![("food".into(), 1800), ("chase".into(), -2)],
                },
            ],
            map: Some(DrawnMap {
                width: 3,
                height: 2,
                cells: vec![0, 1, 0x10, 0, 0, 1],
                outside: Some(0x10),
            }),
            kinds: vec![
                SavedKind {
                    name: "seed".into(),
                    cover: false,
                    needs: names(&["water", "health"]),
                    mems: names(&["lit"]),
                    states: Vec::new(),
                },
                SavedKind {
                    name: "árvore".into(),
                    cover: true,
                    needs: Vec::new(),
                    mems: Vec::new(),
                    states: names(&["grow", "rest"]),
                },
            ],
            scents: names(&["trail"]),
            packs: names(&["/rules/base", "rules/mod"]),
            rules_hash: 0xABCD,
        };
        s.write_meta(&m).unwrap();
        assert_eq!(s.read_meta().unwrap(), Some(m.clone()));
        let none = WorldMeta {
            starts: Vec::new(),
            map: Some(DrawnMap {
                outside: None,
                ..m.map.clone().unwrap()
            }),
            kinds: Vec::new(),
            scents: Vec::new(),
            packs: Vec::new(),
            ..m.clone()
        };
        s.write_meta(&none).unwrap();
        assert_eq!(s.read_meta().unwrap(), Some(none.clone()));
        // A map byte that is no cell is refused.
        let bad_map = WorldMeta {
            map: Some(DrawnMap {
                cells: vec![0, 1, 0x10, 0, 0, 0x22],
                ..m.map.clone().unwrap()
            }),
            ..m.clone()
        };
        s.write_meta(&bad_map).unwrap();
        let err = s.read_meta().unwrap_err().to_string();
        assert!(err.contains("0x22"), "{err}");
        let no_map = WorldMeta { map: None, ..none };
        s.write_meta(&no_map).unwrap();
        assert_eq!(s.read_meta().unwrap(), Some(no_map));
        // A v8 header is refused by version.
        s.write_meta(&m).unwrap();
        let mut bytes = fs::read(s.meta_path()).unwrap();
        bytes[4] = 8;
        fs::write(s.meta_path(), &bytes).unwrap();
        let err = s.read_meta().unwrap_err().to_string();
        assert!(err.contains("format 8, this build reads 9"), "{err}");
        // A header written for a different day length is refused.
        s.write_meta(&m).unwrap();
        let mut bytes = fs::read(s.meta_path()).unwrap();
        bytes[12 + 16] ^= 1; // magic(4) version(4) bits(4) seed(8) tick(8) -> ticks/day
        fs::write(s.meta_path(), &bytes).unwrap();
        let err = s.read_meta().unwrap_err().to_string();
        assert!(err.contains("ticks/day"), "{err}");
        fs::remove_dir_all(s.dir()).unwrap();
    }

    #[test]
    fn chunk_roundtrip_is_bit_exact() {
        let s = tmp_store("chunk");
        let c = ChunkCoord::new(-7, 3);
        let mut data = ChunkData::default();
        generate_chunk(
            &Scenario {
                seed: 3,
                ..Scenario::builtin()
            }
            .terrain(),
            &builtin(),
            c,
            &mut data,
        );
        assert!(!data.actors.rows.is_empty(), "the default density seeds");
        data.cells.feature[0] = Feature::Rock;
        data.cells.ground[CHUNK_CELLS - 1] = Ground::Water;
        let mind = ActorMind {
            uid: 0xDEAD_BEEF_0000_0001,
            born: 7,
            last_think: 9,
            needs: [1, -2, 3, i32::MIN],
            mem: [5; crate::actors::MEM_SLOTS],
            state: 2,
            events: 3,
            hurt: 4,
            hurt_dir: 5,
            _pad: 0,
        };
        let cell = (0..CHUNK_CELLS)
            .find(|&i| data.cells.occupant[i].is_none())
            .unwrap();
        let slot = data.actors_mut().push(cell, SEED, mind);
        data.actors.rows[usize::from(slot)].signal = -300;
        data.actors.rows[usize::from(slot)].look = 2;
        assert!(!s.has_chunk(c));
        assert_eq!(s.read_chunk(c).unwrap(), None);
        s.write_chunk(c, &data.cells, &data.actors, &data.minds, 4242)
            .unwrap();
        assert!(s.has_chunk(c));
        let back = s.read_chunk(c).unwrap().unwrap();
        assert_eq!(back.last_ticked, 4242);
        assert_eq!(back.data, data);
        assert_eq!(back.data.hash(), data.hash());
        assert_eq!(back.data.minds.rows[usize::from(slot)], mind);
        assert_eq!(back.data.cells.occupant[cell], ActorId::pack(SEED, slot));
        assert!(!s.chunk_path(c).with_extension("tmp").exists());
        // An empty chunk round-trips too.
        let e = ChunkCoord::new(0, 0);
        let empty = ChunkData::default();
        s.write_chunk(e, &empty.cells, &empty.actors, &empty.minds, 1)
            .unwrap();
        assert_eq!(s.read_chunk(e).unwrap().unwrap().data, empty);
        fs::remove_dir_all(s.dir()).unwrap();
    }

    #[test]
    fn corrupt_files_are_refused() {
        let s = tmp_store("corrupt");
        let c = ChunkCoord::new(0, 0);
        fs::write(
            s.chunk_path(c),
            b"WMCC\x07\x00\x00\x00\x06\x00\x00\x00 short",
        )
        .unwrap();
        assert!(s.read_chunk(c).is_err());
        let mut data = ChunkData::default();
        generate_chunk(
            &Scenario {
                seed: 1,
                ..Scenario::builtin()
            }
            .terrain(),
            &builtin(),
            c,
            &mut data,
        );
        s.write_chunk(c, &data.cells, &data.actors, &data.minds, 0)
            .unwrap();
        let good = fs::read(s.chunk_path(c)).unwrap();
        let mut bytes = good.clone();
        bytes[28] = 200; // an invalid Ground discriminant (after magic, version, bits, coord, tick)
        fs::write(s.chunk_path(c), &bytes).unwrap();
        assert!(s.read_chunk(c).is_err());
        // Row count that the file does not hold.
        let n_at = 28 + CHUNK_CELLS * (10 + crate::stage::SCENT_CHANNELS);
        let mut bytes = good.clone();
        bytes[n_at..n_at + 4].copy_from_slice(&u32::MAX.to_le_bytes());
        fs::write(s.chunk_path(c), &bytes).unwrap();
        assert!(s.read_chunk(c).unwrap_err().to_string().contains("rows"));
        let mut bytes = good.clone();
        bytes[n_at..n_at + 4].copy_from_slice(&((data.actors.rows.len() + 1) as u32).to_le_bytes());
        fs::write(s.chunk_path(c), &bytes).unwrap();
        assert!(
            s.read_chunk(c)
                .unwrap_err()
                .to_string()
                .contains("truncated")
        );
        // Trailing bytes.
        let mut bytes = good;
        bytes.push(0);
        fs::write(s.chunk_path(c), &bytes).unwrap();
        assert!(
            s.read_chunk(c)
                .unwrap_err()
                .to_string()
                .contains("trailing")
        );
        // A v7 file is refused by version.
        let mut bytes = fs::read(s.chunk_path(c)).unwrap();
        bytes[4] = 7;
        fs::write(s.chunk_path(c), &bytes).unwrap();
        assert!(
            s.read_chunk(c)
                .unwrap_err()
                .to_string()
                .contains("format 7")
        );
        fs::remove_dir_all(s.dir()).unwrap();
    }
}
