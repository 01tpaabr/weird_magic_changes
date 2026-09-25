//! The built-in kinds: `rules/animals.rules`, `rules/grass.rules` and
//! `rules/plants.rules`,
//! compiled at first use in file-name order (the same order `compile_dir`
//! uses, so `WMC_RULES=rules/` gives the same table). The hand-assembled
//! version below is kept as the test oracle for the compiler: what it must
//! produce for the plants file on its own
//!
//! ```text
//! kind seed {
//!   glyph ","  cadence 512
//!   need water  max 1d vital
//!   need health max 1 decay 0 vital
//!   mem lit
//!   when count water within 2 > 0 => water = min(water + 6h, 1d)
//!   when light > 0                => lit += 1
//!   when lit >= 20                => become tree
//! }
//! kind tree {
//!   glyph "T"  cadence 512  sight 2
//!   need water  max 3d vital
//!   need health max 100 decay 0 vital
//!   mem sun
//!   when count water within 2 > 0   => water = min(water + 12h, 3d)
//!   when light > 128 and water > 1d => sun += 1
//!   when sun >= 4 and nearest free within 2 as c => { sun = 0; spawn seed at c }
//! }
//! ```
//!
//! A seed within two cells of water refills, counts lit thinks and becomes a
//! tree after ~20 of them (about a day at cadence 512: 42 thinks a day, half
//! of them lit). A tree counts sunny, well-watered thinks and drops a seed
//! on a free cell within two every ~4 of them (about five a day).

use super::Kinds;

/// The rules text every build carries, in file-name order.
pub const FILES: [(&str, &str); 3] = [
    (
        "animals.rules",
        include_str!("../../../../rules/animals.rules"),
    ),
    ("grass.rules", include_str!("../../../../rules/grass.rules")),
    (
        "plants.rules",
        include_str!("../../../../rules/plants.rules"),
    ),
];

pub const CHICKEN: u16 = 0;
pub const EGG: u16 = 1;
pub const CHICK: u16 = 2;
pub const FOX: u16 = 3;
pub const GRASS: u16 = 4;
pub const SEED: u16 = 5;
pub const TREE: u16 = 6;

pub fn kinds() -> Kinds {
    super::compile::compile_files(&FILES).expect("the built-in rules compile")
}

/// The plants text the hand-assembled program below was written from,
/// frozen here so `rules/plants.rules` can change without touching the
/// compiler's oracle test.
#[cfg(test)]
pub const ORACLE_PLANTS: &str = r##"# Plants. Rules files are compiled at world open into the kind table; see
# docs/ACTORS.md §5 for the language. Kinds are numbered in file-name order,
# then declaration order (this file on its own: `seed` 0, `tree` 1).

kind seed {
  glyph ","
  color "#c9a86a"
  tags plant feed                   # chickens graze `feed`
  cadence 512                       # a think every ~34 game minutes, 42 a day
  sight 2
  food 3h                           # what an eater gains
  place 1 / 100
  need water  max 1d vital          # dries out in a day away from water
  need health max 1 decay 0 vital   # one bite
  mem lit

  when count water within 2 > 0 => water = min(water + 6h, 1d)
  when light > 0                => lit += 1                 # bookkeeping: falls through
  when lit >= 20                => become tree              # ~20 lit thinks: about a day
}

kind tree {
  glyph "T"
  color "#3f9e4d"
  tags plant
  cadence 512
  sight 2
  need water  max 3d vital
  need health max 100 decay 0 vital
  mem sun

  when count water within 2 > 0   => water = min(water + 12h, 3d)
  when light > 128 and water > 1d => sun += 1
  when sun >= 4 and nearest free within 2 as c => { sun = 0; spawn seed at c }    # a seed per ~4 sunny thinks: ~5 a day
}
"##;

/// The plants, assembled by hand. Compiler oracle (see `compile::tests`).
#[cfg(test)]
pub fn hand_assembled() -> Kinds {
    use super::asm::Asm;
    use super::vm::{Action, OpCode, Sense, pred};
    use super::{KindDef, NeedDef};
    use crate::stage::Ground;
    use crate::time::{days, hours};

    // Ids as the plants file compiles on its own.
    const SEED: u16 = 0;
    const TREE: u16 = 1;
    const WATER: u8 = 0;
    const CADENCE_SHIFT: u8 = 9; // 512 ticks
    // Only 3d = 64 800 is beyond a 16-bit immediate: the pool holds it.
    const K_3D: u16 = 0;
    let consts = vec![days(3) as i32];
    let mut a = Asm::new();

    // ---- seed ---------------------------------------------------------------
    let seed_entry = a.here();
    let water = pred::ground(Ground::Water as u8);
    // when count water within 2 > 0 => water = min(water + 6h, 1d)
    let next = a.label();
    a.push(water)
        .push(2)
        .op(OpCode::Count)
        .push(0)
        .op(OpCode::Gt)
        .jz(next);
    a.need(WATER)
        .push(hours(6) as i32)
        .op(OpCode::Add)
        .push(days(1) as i32)
        .op(OpCode::Min)
        .set_need(WATER);
    a.end_rule().bind(next);
    // when light > 0 => lit += 1
    let next = a.label();
    a.sense(Sense::Light).push(0).op(OpCode::Gt).jz(next);
    a.mem(0).push(1).op(OpCode::Add).set_mem(0);
    a.end_rule().bind(next);
    // when lit >= 20 => become tree
    let next = a.label();
    a.mem(0).push(20).op(OpCode::Ge).jz(next);
    a.push(i32::from(TREE)).act(Action::Become);
    a.end_rule().bind(next);
    a.halt();

    // ---- tree ---------------------------------------------------------------
    let tree_entry = a.here();
    // when count water within 2 > 0 => water = min(water + 12h, 3d)
    let next = a.label();
    a.push(water)
        .push(2)
        .op(OpCode::Count)
        .push(0)
        .op(OpCode::Gt)
        .jz(next);
    a.need(WATER)
        .push(hours(12) as i32)
        .op(OpCode::Add)
        .push_k(K_3D)
        .op(OpCode::Min)
        .set_need(WATER);
    a.end_rule().bind(next);
    // when light > 128 and water > 1d => sun += 1
    let next = a.label();
    a.sense(Sense::Light).push(128).op(OpCode::Gt).jz(next);
    a.need(WATER).push(days(1) as i32).op(OpCode::Gt).jz(next);
    a.mem(0).push(1).op(OpCode::Add).set_mem(0);
    a.end_rule().bind(next);
    // when sun >= 4 and nearest free within 2 as c => { sun = 0; spawn seed at c }
    let next = a.label();
    a.mem(0).push(4).op(OpCode::Ge).jz(next);
    a.push(pred::FREE).push(2).nearest(0).jz(next);
    a.push(0).set_mem(0);
    a.push(i32::from(SEED)).load(0).load(1).act(Action::Spawn);
    a.end_rule().bind(next);
    a.halt();

    let code = a.finish();
    let need = |name: &str, max: i32, decays: bool| NeedDef {
        name: name.into(),
        max,
        decays,
        vital: true,
    };
    let defs = vec![
        KindDef {
            id: SEED,
            name: "seed".into(),
            glyph: b',',
            tags: 0b11, // plant, feed
            cadence_shift: CADENCE_SHIFT,
            sight: 2,
            fuel: 512,
            food: hours(3) as i32,
            bite: 1,
            needs: vec![
                need("water", days(1) as i32, true),
                need("health", 1, false),
            ],
            mems: vec!["lit".into()],
            states: 1,
            entry: seed_entry,
            place: super::PLACE_ONE / 100,
            color: 0x00C9_A86A,
            cover: false,
        },
        KindDef {
            id: TREE,
            name: "tree".into(),
            glyph: b'T',
            tags: 0b01, // plant
            cadence_shift: CADENCE_SHIFT,
            sight: 2,
            fuel: 512,
            food: 0,
            bite: 1,
            needs: vec![
                need("water", days(3) as i32, true),
                need("health", 100, false),
            ],
            mems: vec!["sun".into()],
            states: 1,
            entry: tree_entry,
            place: 0,
            color: 0x003F_9E4D,
            cover: false,
        },
    ];
    Kinds::from_parts(defs, code, consts, vec![])
}
