//! Rules: the kind table and the programs that drive actors
//! (`docs/ACTORS.md` §5).
//!
//! [`Kinds`] is one shared, read-only resource: every kind's properties
//! ([`KindDef`]), all bytecode in one `Vec<Op>`, the constant pool and the
//! sub table. [`compile`] builds it from rules text; [`builtin`] is the
//! rules text every build carries (`rules/plants.rules`). Its hash is part
//! of the world's checksum: the rules are an input.

pub mod asm;
pub mod builtin;
pub mod compile;
pub mod vm;

use bevy_ecs::prelude::*;

use crate::actors::{MEM_SLOTS, NEED_SLOTS};
use crate::rng::splitmix64;
use vm::Op;

pub use builtin::{SEED, TREE};
pub use compile::{CompileError, compile, compile_dir, compile_files};

/// One need of a kind: `need NAME max M [decay 0] [vital]`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NeedDef {
    pub name: String,
    pub max: i32,
    /// Loses one per tick (ticks-until-empty) or stays put (points).
    pub decays: bool,
    /// Zero means death.
    pub vital: bool,
}

/// One kind's properties and where its program starts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KindDef {
    /// Its index in the table.
    pub id: u16,
    pub name: String,
    /// Opaque to the sim; the palette draws it.
    pub glyph: u8,
    /// Tag bitset (compiler-assigned).
    pub tags: u64,
    /// Thinks every `1 << cadence_shift` ticks.
    pub cadence_shift: u8,
    /// Search radius cap, in cells (`<= 16`: inside the 3x3 halo).
    pub sight: u8,
    /// Ops per think.
    pub fuel: u32,
    /// What an eater gains, in ticks of food.
    pub food: i32,
    pub bite: u8,
    /// At most [`NEED_SLOTS`].
    pub needs: Vec<NeedDef>,
    /// Memory slot names, at most [`MEM_SLOTS`].
    pub mems: Vec<String>,
    /// Number of `state` blocks (state 0 = none / the first).
    pub states: u8,
    /// Program counter of the think.
    pub entry: u32,
}

impl KindDef {
    pub fn cadence(&self) -> u64 {
        1 << self.cadence_shift
    }

    fn need_index(&self, name: &str) -> Option<usize> {
        self.needs.iter().position(|n| n.name == name)
    }

    fn mem_index(&self, name: &str) -> Option<usize> {
        self.mems.iter().position(|m| m == name)
    }
}

/// How a row's slots carry over `become` from one kind to another: for each
/// slot of the new kind, the old slot with the same name, or `NONE`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Remap {
    pub needs: [u8; NEED_SLOTS],
    pub mems: [u8; MEM_SLOTS],
}

impl Remap {
    pub const NONE: u8 = u8::MAX;

    fn between(from: &KindDef, to: &KindDef) -> Self {
        let mut r = Remap {
            needs: [Self::NONE; NEED_SLOTS],
            mems: [Self::NONE; MEM_SLOTS],
        };
        for (i, n) in to.needs.iter().enumerate() {
            if let Some(j) = from.need_index(&n.name) {
                r.needs[i] = j as u8;
            }
        }
        for (i, m) in to.mems.iter().enumerate() {
            if let Some(j) = from.mem_index(m) {
                r.mems[i] = j as u8;
            }
        }
        r
    }
}

/// The kind table and every program. `row.kind` indexes `defs`. Saved by
/// name in `world.wmc`; a save whose names do not match this build is
/// refused.
#[derive(Resource, Debug, Clone, PartialEq, Eq)]
pub struct Kinds {
    pub defs: Vec<KindDef>,
    /// `defs[i].glyph`, for the renderer's per-cell lookup.
    pub glyphs: Vec<u8>,
    pub code: Vec<Op>,
    pub consts: Vec<i32>,
    /// Entry pc per sub, indexed by `Call imm`.
    pub subs: Vec<u32>,
    /// Hash of everything above: the rules as an input to the checksum.
    pub hash: u64,
    /// `remaps[from * defs.len() + to]`.
    remaps: Vec<Remap>,
}

impl Kinds {
    pub fn from_parts(
        mut defs: Vec<KindDef>,
        code: Vec<Op>,
        consts: Vec<i32>,
        subs: Vec<u32>,
    ) -> Self {
        assert!(defs.len() < usize::from(u16::MAX), "too many kinds");
        for (i, d) in defs.iter_mut().enumerate() {
            d.id = i as u16;
            assert!(d.needs.len() <= NEED_SLOTS, "{}: too many needs", d.name);
            assert!(d.mems.len() <= MEM_SLOTS, "{}: too many mem slots", d.name);
            assert!(d.sight <= 16, "{}: sight beyond the halo", d.name);
            assert!(d.cadence_shift < 32, "{}: cadence", d.name);
        }
        let glyphs = defs.iter().map(|d| d.glyph).collect();
        let n = defs.len();
        let remaps = (0..n * n)
            .map(|i| Remap::between(&defs[i / n], &defs[i % n]))
            .collect();
        let hash = hash_all(&defs, &code, &consts, &subs);
        Self {
            defs,
            glyphs,
            code,
            consts,
            subs,
            hash,
            remaps,
        }
    }

    /// The kinds this build knows.
    pub fn builtin() -> Self {
        builtin::kinds()
    }

    pub fn len(&self) -> usize {
        self.defs.len()
    }

    pub fn is_empty(&self) -> bool {
        self.defs.is_empty()
    }

    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.defs.iter().map(|d| d.name.as_str())
    }

    pub fn by_name(&self, name: &str) -> Option<&KindDef> {
        self.defs.iter().find(|d| d.name == name)
    }

    #[inline]
    pub fn def(&self, kind: u16) -> &KindDef {
        &self.defs[usize::from(kind)]
    }

    #[inline]
    pub fn remap(&self, from: u16, to: u16) -> &Remap {
        &self.remaps[usize::from(from) * self.defs.len() + usize::from(to)]
    }
}

fn hash_all(defs: &[KindDef], code: &[Op], consts: &[i32], subs: &[u32]) -> u64 {
    let mut h = 0x0052_656C_6573_u64; // "Rules"
    let mut mix = |v: u64| h = splitmix64(h ^ v);
    for d in defs {
        for b in d.name.bytes() {
            mix(u64::from(b));
        }
        mix(u64::from(d.glyph)
            | u64::from(d.cadence_shift) << 8
            | u64::from(d.sight) << 16
            | u64::from(d.bite) << 24);
        mix(d.tags);
        mix(u64::from(d.fuel) | u64::from(d.food as u32) << 32);
        mix(u64::from(d.entry) | u64::from(d.states) << 32);
        for n in &d.needs {
            for b in n.name.bytes() {
                mix(u64::from(b));
            }
            mix(u64::from(n.max as u32) | u64::from(n.decays) << 32 | u64::from(n.vital) << 33);
        }
        for m in &d.mems {
            for b in m.bytes() {
                mix(u64::from(b));
            }
            mix(0xFF);
        }
    }
    for op in code {
        mix(u64::from(op.bits()));
    }
    for &c in consts {
        mix(u64::from(c as u32) | 1 << 40);
    }
    for &s in subs {
        mix(u64::from(s) | 1 << 41);
    }
    h
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_table_has_seed_then_tree_and_a_stable_hash() {
        let k = Kinds::builtin();
        assert_eq!(k.names().collect::<Vec<_>>(), vec!["seed", "tree"]);
        assert_eq!(k.def(SEED).id, SEED);
        assert_eq!(k.def(TREE).id, TREE);
        assert_eq!(k.glyphs, vec![b',', b'T']);
        assert_eq!(k.hash, Kinds::builtin().hash);
        assert!(!k.is_empty() && k.len() == 2);
        let mut other = Kinds::builtin();
        other.defs[0].glyph = b'x';
        assert_ne!(
            hash_all(&other.defs, &other.code, &other.consts, &other.subs),
            k.hash
        );
    }

    #[test]
    fn remap_matches_slots_by_name() {
        let k = Kinds::builtin();
        // seed: water, health; mem lit. tree: water, health; mem sun.
        let r = k.remap(SEED, TREE);
        assert_eq!(r.needs[..2], [0, 1]);
        assert_eq!(r.needs[2], Remap::NONE);
        assert!(r.mems.iter().all(|&m| m == Remap::NONE));
        assert_eq!(k.remap(SEED, SEED).mems[0], 0);
    }
}
