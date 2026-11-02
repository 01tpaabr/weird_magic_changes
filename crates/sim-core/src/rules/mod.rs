//! Rules: the kind table and the programs that drive actors
//! (`docs/ACTORS.md` §5).
//!
//! [`Kinds`] is one shared, read-only resource: every kind's properties
//! ([`KindDef`]), all bytecode in one `Vec<Op>`, the constant pool and the
//! sub table. [`compile`] builds it from rules text; [`builtin`] is the
//! rules text every build carries (`rules/*.rules`, see `builtin::FILES`). Its hash is part
//! of the world's checksum: the rules are an input.

pub mod asm;
pub mod builtin;
pub mod compile;
#[cfg(test)]
pub mod gen_rules;
pub mod law;
pub mod vm;

use bevy_ecs::prelude::*;

use crate::actors::{MEM_SLOTS, NEED_SLOTS};
use crate::rng::splitmix64;
use crate::stage::{Feature, Ground, MAX_FEATURES, MAX_GROUNDS, MaterialTable};
use vm::Op;

pub use builtin::{BEE, CHICK, CHICKEN, EGG, FLOWER, FOX, GRASS, HIVE, SEED, TREE};
pub use compile::{
    CompileError, ConstSet, compile, compile_files, compile_files_with, compile_packs,
    compile_packs_with,
};

/// One need of a kind: `need NAME max M [decay 0|1] [vital]`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NeedDef {
    pub name: String,
    pub max: i32,
    /// Loses one per tick (ticks-until-empty) or stays put (points).
    pub decays: bool,
    /// Zero means death.
    pub vital: bool,
}

/// Who may write a field (`docs/WORLD.md` §4): any write (`add`,
/// `harvest`, `drop`), only conserved transfers, or none from actors.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Policy {
    #[default]
    Free,
    /// No `add`: nothing is made or destroyed but by moving it.
    Conserved,
    /// No writes from actors at all.
    Fixed,
}

/// Where a field's cells start when a chunk is made: a pure function of
/// the seed and the cell.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FieldInit {
    /// Every cell at this value.
    Value(u16),
    /// Value noise of this scale (in cells), mapped onto `0..=max`.
    Noise(u32),
}

/// One field: a number in every cell (`field NAME max M`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FieldDef {
    pub name: String,
    /// Values run `0..=max`.
    pub max: u16,
    pub policy: Policy,
    pub init: FieldInit,
    /// `0xRRGGBB` the renderer tints a cell toward, by value; `None`: not
    /// drawn.
    pub color: Option<u32>,
    /// What its law reads where nothing is loaded; `None`: the chunk's own
    /// edge.
    pub outside: Option<u16>,
    /// `relief`: the renderer shades its slopes (a height).
    pub relief: bool,
    /// Its law, if it has one.
    pub law: Option<law::Law>,
}

/// A ground or a feature, as the rules declare it (`ground NAME { ... }`,
/// `feature NAME { ... }`, `docs/WORLD.md` §5).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MaterialDef {
    pub name: String,
    /// What the renderer draws on a bare cell of it.
    pub glyph: u8,
    /// `0xRRGGBB`; `None`: the palette's own (the built-in three).
    pub color: Option<u32>,
    /// A ground: walkable. A feature: blocks.
    pub solid: bool,
    /// A ground: `drink` refills `water` from it.
    pub drink: bool,
}

/// Every ground and feature of a rule set, the engine's three first (soil
/// and water, ground 0 and 1; rock, feature 1), and the table the sim reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Materials {
    pub grounds: Vec<MaterialDef>,
    /// `features[i]` is `Feature(i + 1)`: 0 is none.
    pub features: Vec<MaterialDef>,
    pub table: MaterialTable,
}

impl Default for Materials {
    fn default() -> Self {
        let m = |name: &str, glyph: u8, solid: bool, drink: bool| MaterialDef {
            name: name.into(),
            glyph,
            color: None,
            solid,
            drink,
        };
        Self {
            grounds: vec![m("soil", b'.', true, false), m("water", b'~', false, true)],
            features: vec![m("rock", b'#', true, false)],
            table: MaterialTable::DEFAULT,
        }
    }
}

impl Materials {
    /// The table the sim reads, from the definitions.
    pub fn table_of(grounds: &[MaterialDef], features: &[MaterialDef]) -> MaterialTable {
        let mut t = MaterialTable {
            walk: [false; 256],
            drink: [false; 256],
            blocks: [true; 256],
        };
        t.blocks[0] = false;
        for (i, g) in grounds.iter().enumerate().take(MAX_GROUNDS) {
            (t.walk[i], t.drink[i]) = (g.solid, g.drink);
        }
        for (i, f) in features.iter().enumerate().take(MAX_FEATURES) {
            t.blocks[i + 1] = f.solid;
        }
        t
    }

    /// The ground named `name`.
    pub fn ground(&self, name: &str) -> Option<Ground> {
        let i = self.grounds.iter().position(|g| g.name == name)?;
        Some(Ground(
            u8::try_from(i).expect("at most MAX_GROUNDS grounds"),
        ))
    }

    /// The feature named `name`.
    pub fn feature(&self, name: &str) -> Option<Feature> {
        let i = self.features.iter().position(|f| f.name == name)?;
        Some(Feature(
            u8::try_from(i + 1).expect("at most MAX_FEATURES features"),
        ))
    }
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
    /// Search radius cap, in cells (at most [`compile::MAX_SIGHT`]: inside
    /// the 3x3 halo).
    pub sight: u8,
    /// Ops per think.
    pub fuel: u32,
    /// What an eater gains, in ticks of food.
    pub food: i32,
    pub bite: u8,
    /// How far `eat`, `hit` and `graze` reach, in cells (1: a neighbour).
    pub reach: u8,
    /// `climb N on F`: `move` refuses a step that rises more than N in
    /// field F (its index).
    pub climb: Option<(u8, i32)>,
    /// At most [`NEED_SLOTS`].
    pub needs: Vec<NeedDef>,
    /// Memory slot names, at most [`MEM_SLOTS`].
    pub mems: Vec<String>,
    /// Number of `state` blocks (state 0 = none / the first).
    pub states: u8,
    /// Program counter of the think.
    pub entry: u32,
    /// `0xRRGGBB`. Opaque to the sim, like `glyph`: only the palette reads it.
    pub color: u32,
    /// Ground cover: lives in the cell's `cover` layer, never blocks, lies
    /// under whoever stands there; eaten with `graze`, not `eat`.
    pub cover: bool,
    /// Drawn as a whole cell of its colour, no glyph (`solid`). Opaque to
    /// the sim, like `color`.
    pub solid: bool,
    /// The concrete kind this one `extends`, if any. Kinds are numbered in
    /// pre-order over this tree, so a kind's family (itself and every kind
    /// that extends it) is the id range `id..Kinds::family_end[id]`.
    pub parent: Option<u16>,
}

/// Where a rule's code sits, for `wmc why`: not part of the program, not
/// hashed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuleInfo {
    pub kind: u16,
    /// The `state` block it is in, `None` for a reflex.
    pub state: Option<u8>,
    /// Index into [`DebugInfo::files`], and the line of its `when`.
    pub file: u16,
    pub line: u32,
    /// That source line, trimmed.
    pub text: String,
    /// First op of the condition, and of the body: a think that reaches
    /// `body_pc` fired the rule.
    pub cond_pc: u32,
    pub body_pc: u32,
    /// The terms of its condition's top-level `and`, as written, in order
    /// (one term for a condition whose top level is an `or`). A think
    /// that does not fire the rule stopped at the last term it entered:
    /// that term was false.
    pub terms: Vec<Term>,
    /// The trait or ancestor kind the rule was inherited from; `None` for
    /// the kind's own rules.
    pub via: Option<String>,
    /// What it does and how often, read off its source (`wmc rates`).
    pub shape: RuleShape,
}

/// One term of a rule's condition: its first op, and its source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Term {
    pub pc: u32,
    pub text: String,
}

/// What a rule does and how often, read off its source without running it
/// (`wmc rates`): its dice, its other gates, the age it waits for, and the
/// action its body takes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuleShape {
    /// The odds a think passes its dice, as a fraction: the terms of its
    /// condition's top-level `and` that are `chance(p)` or `rand(n) < k`
    /// (or `<=`) with constant arguments, multiplied. `(1, 1)` without dice.
    pub dice: (u64, u64),
    /// The condition's other top-level terms: gates whose odds depend on
    /// the world (a search, a need, the hour).
    pub gates: u16,
    /// An `age > A` (or `>=`) term: the age it waits for, in ticks.
    pub after_age: Option<i32>,
    /// What its body does at the top level, in order, the first that ends
    /// the think: `die`, `become chick`, `spawn fox`, `eat`, `move`,
    /// `idle`, `next HOME`, `wander()` (a sub, which may act), `if` (an
    /// action in a branch); `None` when it only writes memory or sets
    /// effects, and the think goes on.
    pub acts: Option<String>,
}

impl Default for RuleShape {
    fn default() -> Self {
        Self {
            dice: (1, 1),
            gates: 0,
            after_age: None,
            acts: None,
        }
    }
}

impl RuleShape {
    /// The chance a think passes the dice.
    pub fn dice_odds(&self) -> f64 {
        self.dice.0 as f64 / self.dice.1.max(1) as f64
    }
}

/// How serious a [`Diagnostic`] is. Errors stop the compile and are
/// [`compile::CompileError`]s; these do not.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    /// A likely mistake: the rules compile and run.
    Warning,
    /// Information worth knowing.
    Note,
}

/// A warning or note about a rule set that compiled (`wmc lint` prints
/// them as `file:line:col: warning: message`, or `file: warning: message`
/// when `line` is 0).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagnostic {
    pub level: Level,
    pub file: String,
    pub line: u32,
    pub col: u32,
    pub msg: String,
}

impl std::fmt::Display for Diagnostic {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let level = match self.level {
            Level::Warning => "warning",
            Level::Note => "note",
        };
        if self.line == 0 {
            // Not about a line (a whole file, or a scenario start).
            return write!(f, "{}: {level}: {}", self.file, self.msg);
        }
        write!(
            f,
            "{}:{}:{}: {level}: {}",
            self.file, self.line, self.col, self.msg
        )
    }
}

/// Names and positions the compiler knows and the programs do not need:
/// what `wmc why` prints. Empty for hand-assembled kinds.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DebugInfo {
    pub files: Vec<String>,
    /// Every rule of every kind, in code order.
    pub rules: Vec<RuleInfo>,
    /// State names per kind, in state order.
    pub states: Vec<Vec<String>>,
    /// Sub names, indexed like [`Kinds::subs`] (a member sub as
    /// `kind::sub`).
    pub subs: Vec<String>,
    /// Trait names, in file then declaration order.
    pub traits: Vec<String>,
    /// Per kind: its direct parents as written (`drinker(90min)`).
    pub parents: Vec<Vec<String>>,
    /// Warnings and notes about the rule set, in kind order.
    pub diagnostics: Vec<Diagnostic>,
    /// The packs these rules were compiled from, as absolute paths in
    /// order (empty: the built-in rules). A save remembers them.
    pub packs: Vec<String>,
    /// Per kind: the kinds its code may `spawn` or `become` (the author
    /// lint's "never appears", `Scenario::unseen`).
    pub makes: Vec<Vec<u16>>,
    /// Per kind: where it is declared, as `(file index, line, column)`.
    pub kind_at: Vec<(u16, u32, u32)>,
}

/// Colour of a kind that declares none: the palette's old actor yellow.
pub const DEFAULT_COLOR: u32 = 0x00FF_F39C;

impl KindDef {
    pub fn cadence(&self) -> u64 {
        1 << self.cadence_shift
    }

    /// Slot of the need called `name`, if this kind has one.
    pub fn need_named(&self, name: &str) -> Option<usize> {
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
            if let Some(j) = from.need_named(&n.name) {
                r.needs[i] = u8::try_from(j).expect("at most NEED_SLOTS needs");
            }
        }
        for (i, m) in to.mems.iter().enumerate() {
            if let Some(j) = from.mem_index(m) {
                r.mems[i] = u8::try_from(j).expect("at most MEM_SLOTS mems");
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
    /// `defs[i].color`, for the renderer.
    pub colors: Vec<u32>,
    /// `defs[i].solid`, for the renderer.
    pub solids: Vec<bool>,
    /// `defs[i].tags`, for tag predicates in the VM's halo.
    pub tag_bits: Vec<u64>,
    /// One past the last kind of each kind's family (see
    /// [`KindDef::parent`]): a kind predicate matches `k..family_end[k]`.
    pub family_end: Vec<u16>,
    pub code: Vec<Op>,
    pub consts: Vec<i32>,
    /// Entry pc per sub, indexed by `Call imm`.
    pub subs: Vec<u32>,
    /// The fields, in declaration order: `ChunkCells::fields[i]` is
    /// `fields[i]`'s layer.
    pub fields: Vec<FieldDef>,
    /// Grounds and features: what a cell's `ground` and `feature` index.
    pub materials: Materials,
    /// Source positions and names, for `wmc why`.
    pub debug: DebugInfo,
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
            d.id = u16::try_from(i).expect("fewer than u16::MAX kinds, asserted above");
            assert!(d.needs.len() <= NEED_SLOTS, "{}: too many needs", d.name);
            assert!(d.mems.len() <= MEM_SLOTS, "{}: too many mem slots", d.name);
            assert!(
                d.sight <= compile::MAX_SIGHT,
                "{}: sight beyond the halo",
                d.name
            );
            assert!(d.cadence_shift < 32, "{}: cadence", d.name);
        }
        let glyphs = defs.iter().map(|d| d.glyph).collect();
        let tag_bits = defs.iter().map(|d| d.tags).collect();
        // Families: pre-order numbering puts every descendant right after
        // its ancestor, so a family is one id range.
        let last = u16::try_from(defs.len()).expect("fewer than u16::MAX kinds");
        let mut family_end: Vec<u16> = (1..=last).collect();
        for id in (0..defs.len()).rev() {
            if let Some(p) = defs[id].parent {
                assert!(
                    usize::from(p) < id,
                    "{}: a parent is numbered before its children",
                    defs[id].name
                );
                family_end[usize::from(p)] = family_end[usize::from(p)].max(family_end[id]);
            }
        }
        let colors = defs.iter().map(|d| d.color).collect();
        let solids = defs.iter().map(|d| d.solid).collect();
        let n = defs.len();
        let remaps = (0..n * n)
            .map(|i| Remap::between(&defs[i / n], &defs[i % n]))
            .collect();
        let hash = hash_all(&defs, &code, &consts, &subs);
        Self {
            defs,
            glyphs,
            colors,
            solids,
            tag_bits,
            family_end,
            code,
            consts,
            subs,
            fields: Vec::new(),
            materials: Materials::default(),
            debug: DebugInfo::default(),
            hash,
            remaps,
        }
    }

    /// The same rules with source positions for `wmc why` (not hashed).
    pub fn with_debug(mut self, debug: DebugInfo) -> Self {
        self.debug = debug;
        self
    }

    /// The same rules with these fields (folded into the hash; none leaves
    /// it as it was).
    pub fn with_fields(mut self, fields: Vec<FieldDef>) -> Self {
        for f in &fields {
            let mut h = self.hash ^ 0xF1E1D;
            for b in f.name.bytes() {
                h = splitmix64(h ^ u64::from(b));
            }
            let (init, scale) = match f.init {
                FieldInit::Value(v) => (0, u64::from(v)),
                FieldInit::Noise(s) => (1, u64::from(s)),
            };
            for v in [
                u64::from(f.max),
                f.policy as u64,
                init,
                scale,
                f.color.map_or(u64::MAX, u64::from),
                f.outside.map_or(u64::MAX, u64::from),
                u64::from(f.relief),
            ] {
                h = splitmix64(h ^ v);
            }
            for v in f.law.iter().flat_map(law::Law::words) {
                h = splitmix64(h ^ v);
            }
            self.hash = splitmix64(h);
        }
        self.fields = fields;
        self
    }

    /// The same rules with these materials (folded into the hash when they
    /// are not the engine's three as they come).
    pub fn with_materials(mut self, materials: Materials) -> Self {
        if materials != Materials::default() {
            let mut h = self.hash ^ 0x7E44A1;
            for (k, m) in materials
                .grounds
                .iter()
                .chain(&materials.features)
                .enumerate()
            {
                h = splitmix64(h ^ k as u64);
                for b in m.name.bytes() {
                    h = splitmix64(h ^ u64::from(b));
                }
                for v in [
                    u64::from(m.glyph),
                    m.color.map_or(u64::MAX, u64::from),
                    u64::from(m.solid),
                    u64::from(m.drink),
                ] {
                    h = splitmix64(h ^ v);
                }
            }
            self.hash = splitmix64(h ^ materials.grounds.len() as u64);
        }
        self.materials = materials;
        self
    }

    /// The index of field `name`.
    pub fn field_named(&self, name: &str) -> Option<usize> {
        self.fields.iter().position(|f| f.name == name)
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
            | u64::from(d.bite) << 24
            | u64::from(d.reach) << 32);
        if let Some((f, n)) = d.climb {
            mix(u64::from(f) << 32 | u64::from(n as u32));
        }
        mix(d.tags);
        mix(u64::from(d.color) | 1 << 43);
        mix(u64::from(d.cover) | 1 << 44);
        if d.solid {
            mix(1 << 46);
        }
        mix(u64::from(d.fuel) | u64::from(d.food as u32) << 32);
        mix(u64::from(d.entry) | u64::from(d.states) << 32);
        if let Some(p) = d.parent {
            mix(u64::from(p) | 1 << 45);
        }
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
    fn builtin_table_is_in_preorder_with_a_stable_hash() {
        let k = Kinds::builtin();
        // File-name order, a chick right after the chicken it extends.
        let names = [
            "chicken", "chick", "egg", "fox", "flower", "hive", "bee", "grass", "seed", "tree",
        ];
        assert_eq!(k.names().collect::<Vec<_>>(), names);
        for (id, kind) in [
            CHICKEN, CHICK, EGG, FOX, FLOWER, HIVE, BEE, GRASS, SEED, TREE,
        ]
        .into_iter()
        .enumerate()
        {
            assert_eq!(usize::from(kind), id);
            assert_eq!(k.def(kind).name, names[id]);
        }
        assert_eq!(k.glyphs, b"Cco\x46*Hb',T".to_vec());
        assert_eq!(k.def(CHICK).parent, Some(CHICKEN));
        assert_eq!(k.family_end[usize::from(CHICKEN)], CHICK + 1);
        assert_eq!(
            k.debug.traits,
            ["fowl", "walker", "drinker", "rooted", "mortal"]
        );
        assert_eq!(
            k.field_named("trail"),
            Some(0),
            "the bees' trail is a field"
        );
        assert_eq!(k.def(BEE).states, 4);
        assert_eq!(k.colors[usize::from(FOX)], 0x00E8_792B);
        assert_eq!(k.hash, Kinds::builtin().hash);
        assert_eq!(
            k.def(CHICKEN).need_named("water"),
            Some(0),
            "the drinker's, first"
        );
        assert_eq!(k.def(TREE).need_named("food"), None);
        // Tags in first-appearance order: animal, meat, plant, feed.
        assert_eq!(
            k.tag_bits,
            vec![
                0b0011, 0b0011, 0b0010, 0b0001, 0b0100, 0, 0b0001, 0b1100, 0b1100, 0b0100
            ]
        );
        let mut other = Kinds::builtin();
        other.defs[0].color = 0;
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
