//! Persistence: a save directory holding world metadata plus one file per
//! **modified** chunk. Clean chunks are regenerated from the seed, so a save
//! of an unexplored world is a few dozen bytes.
//!
//! ```text
//! <dir>/world.wmc              magic, version, seed, tick, initial size, gen params
//! <dir>/chunks/<x>_<y>.wmcc    magic, version, coord, then each layer as raw bytes
//! ```
//!
//! Everything is little-endian, fixed layout, written to a temp file and
//! renamed into place (a crash mid-write leaves the old file intact). No
//! serde: the layout is `ChunkCells` verbatim, so a save is a memcpy.
//! Bump [`FORMAT_VERSION`] whenever a layer, `CHUNK_BITS`, or `GenParams`
//! changes; old saves are refused rather than misread.
//!
//! Revisit when a save directory grows past a few thousand chunk files:
//! pack chunks into region files (32x32 chunks per file with an offset table).

use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use crate::stage::worldgen::GenParams;
use crate::stage::{CHUNK_BITS, CHUNK_CELLS, ChunkCells, ChunkCoord};

pub const FORMAT_VERSION: u32 = 1;
const WORLD_MAGIC: &[u8; 4] = b"WMCW";
const CHUNK_MAGIC: &[u8; 4] = b"WMCC";

/// World-level facts that must survive a restart.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WorldMeta {
    pub seed: u64,
    pub tick: u64,
    /// Size of the initially generated region, in cells, at `[0, w) x [0, h)`.
    pub initial_width: u32,
    pub initial_height: u32,
    pub params: GenParams,
}

/// A save directory. Cheap to clone; holds no open files.
#[derive(Debug, Clone)]
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
        let meta = WorldMeta {
            seed: r.u64()?,
            tick: r.u64()?,
            initial_width: r.u32()?,
            initial_height: r.u32()?,
            params: GenParams {
                water_scale: r.f32()?,
                water_level: r.f32()?,
                rock_on_soil: r.f32()?,
                rock_on_water: r.f32()?,
            },
        };
        r.finish()?;
        Ok(Some(meta))
    }

    pub fn write_meta(&self, m: &WorldMeta) -> io::Result<()> {
        let mut w = Writer::new(WORLD_MAGIC);
        w.u64(m.seed);
        w.u64(m.tick);
        w.u32(m.initial_width);
        w.u32(m.initial_height);
        w.f32(m.params.water_scale);
        w.f32(m.params.water_level);
        w.f32(m.params.rock_on_soil);
        w.f32(m.params.rock_on_water);
        write_atomic(&self.meta_path(), &w.buf)
    }

    pub fn has_chunk(&self, c: ChunkCoord) -> bool {
        self.chunk_path(c).is_file()
    }

    /// `Ok(None)` if the chunk was never saved (regenerate it).
    pub fn read_chunk(&self, c: ChunkCoord) -> io::Result<Option<ChunkCells>> {
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
        let mut cells = ChunkCells::default();
        let ground = r.bytes(CHUNK_CELLS)?;
        let feature = r.bytes(CHUNK_CELLS)?;
        let occupant = r.bytes(CHUNK_CELLS * 4)?;
        cells.ground.copy_from_slice(
            bytemuck::checked::try_cast_slice(ground).map_err(|e| bad(e.to_string()))?,
        );
        cells.feature.copy_from_slice(
            bytemuck::checked::try_cast_slice(feature).map_err(|e| bad(e.to_string()))?,
        );
        // Occupant ids are u32 LE; on LE targets this is a memcpy.
        for (o, b) in cells.occupant.iter_mut().zip(occupant.chunks_exact(4)) {
            o.0 = u32::from_le_bytes([b[0], b[1], b[2], b[3]]);
        }
        r.finish()?;
        Ok(Some(cells))
    }

    pub fn write_chunk(&self, c: ChunkCoord, cells: &ChunkCells) -> io::Result<()> {
        let mut w = Writer::new(CHUNK_MAGIC);
        w.i32(c.x);
        w.i32(c.y);
        w.buf.extend_from_slice(bytemuck::cast_slice(&cells.ground));
        w.buf
            .extend_from_slice(bytemuck::cast_slice(&cells.feature));
        for o in &cells.occupant {
            w.buf.extend_from_slice(&o.0.to_le_bytes());
        }
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
        };
        s.write_meta(&m).unwrap();
        assert_eq!(s.read_meta().unwrap(), Some(m));
        fs::remove_dir_all(s.dir()).unwrap();
    }

    #[test]
    fn chunk_roundtrip_is_bit_exact() {
        let s = tmp_store("chunk");
        let c = ChunkCoord::new(-7, 3);
        let mut cells = ChunkCells::default();
        generate_chunk(3, &GenParams::default(), c, &mut cells);
        cells.occupant[100] = ActorId(0xABCD);
        cells.feature[0] = Feature::Rock;
        cells.ground[CHUNK_CELLS - 1] = Ground::Water;
        assert!(!s.has_chunk(c));
        assert_eq!(s.read_chunk(c).unwrap(), None);
        s.write_chunk(c, &cells).unwrap();
        assert!(s.has_chunk(c));
        let back = s.read_chunk(c).unwrap().unwrap();
        assert_eq!(back.hash(), cells.hash());
        assert_eq!(back.occupant[100], ActorId(0xABCD));
        assert!(!s.chunk_path(c).with_extension("tmp").exists());
        fs::remove_dir_all(s.dir()).unwrap();
    }

    #[test]
    fn corrupt_files_are_refused() {
        let s = tmp_store("corrupt");
        let c = ChunkCoord::new(0, 0);
        fs::write(
            s.chunk_path(c),
            b"WMCC\x01\x00\x00\x00\x06\x00\x00\x00 short",
        )
        .unwrap();
        assert!(s.read_chunk(c).is_err());
        let mut cells = ChunkCells::default();
        generate_chunk(1, &GenParams::default(), c, &mut cells);
        s.write_chunk(c, &cells).unwrap();
        let mut bytes = fs::read(s.chunk_path(c)).unwrap();
        bytes[20] = 200; // an invalid Ground discriminant
        fs::write(s.chunk_path(c), &bytes).unwrap();
        assert!(s.read_chunk(c).is_err());
        fs::remove_dir_all(s.dir()).unwrap();
    }
}
