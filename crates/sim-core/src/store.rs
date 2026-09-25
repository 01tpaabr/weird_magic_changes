//! Persistence: a save directory holding world metadata plus one file per
//! **modified** chunk. Clean chunks are regenerated from the seed, so a save
//! of an unexplored world is a few dozen bytes.
//!
//! ```text
//! <dir>/world.wmc              magic, version, seed, tick, ticks/day, initial size, gen params, kind names, rules hash
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
//! length must not open it. Kind names travel by name so rows can be
//! checked against the build's kind table on open (`sim::open`).
//!
//! Revisit when a save directory grows past a few thousand chunk files:
//! pack chunks into region files (32x32 chunks per file with an offset table).

use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use bevy_ecs::resource::Resource;

use crate::actors::{ActorMind, ActorPub, ChunkActors, ChunkMinds};
use crate::stage::worldgen::GenParams;
use crate::stage::{CHUNK_BITS, CHUNK_CELLS, ChunkCells, ChunkCoord, ChunkData};
use crate::time::TICKS_PER_DAY;

pub const FORMAT_VERSION: u32 = 8;
const WORLD_MAGIC: &[u8; 4] = b"WMCW";
const CHUNK_MAGIC: &[u8; 4] = b"WMCC";

/// World-level facts that must survive a restart.
#[derive(Debug, Clone, PartialEq)]
pub struct WorldMeta {
    pub seed: u64,
    pub tick: u64,
    /// Size of the initially generated region, in cells, at `[0, w) x [0, h)`.
    pub initial_width: u32,
    pub initial_height: u32,
    pub params: GenParams,
    /// Kind table of the build that wrote the save, by index.
    pub kinds: Vec<String>,
    /// `Kinds::hash` of the rules the save was last played with. Recorded,
    /// not enforced: rules may be tuned between sessions; the checksum
    /// says when they were.
    pub rules_hash: u64,
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
        let meta = WorldMeta {
            seed,
            tick,
            initial_width: r.u32()?,
            initial_height: r.u32()?,
            params: GenParams {
                water_scale: r.f32()?,
                water_level: r.f32()?,
                rock_on_soil: r.f32()?,
                rock_on_water: r.f32()?,
            },
            kinds: Vec::new(),
            rules_hash: 0,
        };
        let n = r.u32()?;
        if n > u32::from(u16::MAX) {
            return Err(bad(format!("{n} kinds")));
        }
        let mut kinds = Vec::with_capacity(n as usize);
        for _ in 0..n {
            let len = r.u32()?;
            let name = r.bytes(len as usize)?;
            kinds.push(String::from_utf8(name.to_vec()).map_err(|e| bad(e.to_string()))?);
        }
        let rules_hash = r.u64()?;
        r.finish()?;
        Ok(Some(WorldMeta {
            kinds,
            rules_hash,
            ..meta
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
        w.u32(u32::try_from(m.kinds.len()).expect("kind count fits u32"));
        for k in &m.kinds {
            w.u32(u32::try_from(k.len()).expect("kind name fits u32"));
            w.buf.extend_from_slice(k.as_bytes());
        }
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
    fn u32(&mut self, v: u32) {
        self.buf.extend_from_slice(&v.to_le_bytes());
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
    fn u32(&mut self) -> io::Result<u32> {
        let b = self.bytes(4)?;
        Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
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
    use crate::rules::{Kinds, SEED};
    use crate::stage::worldgen::generate_chunk;
    use crate::stage::{ActorId, Feature, Ground};

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
        let m = WorldMeta {
            seed: 0xDEAD_BEEF,
            tick: 12,
            initial_width: 300,
            initial_height: 200,
            params: GenParams {
                water_scale: 9.5,
                ..GenParams::default()
            },
            kinds: vec!["seed".into(), "árvore".into()],
            rules_hash: 0xABCD,
        };
        s.write_meta(&m).unwrap();
        assert_eq!(s.read_meta().unwrap(), Some(m.clone()));
        let none = WorldMeta {
            kinds: Vec::new(),
            ..m
        };
        s.write_meta(&none).unwrap();
        assert_eq!(s.read_meta().unwrap(), Some(none));
        // A header written for a different day length is refused.
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
        generate_chunk(3, &GenParams::default(), &Kinds::builtin(), c, &mut data);
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
        generate_chunk(1, &GenParams::default(), &Kinds::builtin(), c, &mut data);
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
