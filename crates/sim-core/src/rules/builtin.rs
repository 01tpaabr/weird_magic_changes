//! The built-in kinds, hand-assembled. What the compiler must produce for:
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
//!   when sun >= 40 and nearest free within 2 as c => { sun = 0; spawn seed at c }
//! }
//! ```
//!
//! A seed within two cells of water refills, counts lit thinks and becomes a
//! tree after ~20 of them (about a day at cadence 512: 42 thinks a day, half
//! of them lit). A tree counts sunny, well-watered thinks and drops a seed
//! on a free cell within two every ~40 of them (about two days).

use super::asm::Asm;
use super::vm::{Action, OpCode, Sense, pred};
use super::{KindDef, Kinds, NeedDef};
use crate::stage::Ground;
use crate::time::{days, hours};

pub const SEED: u16 = 0;
pub const TREE: u16 = 1;

const WATER: u8 = 0;
const CADENCE_SHIFT: u8 = 9; // 512 ticks

/// Constant pool indices.
const K_6H: u16 = 0;
const K_1D: u16 = 1;
const K_12H: u16 = 2;
const K_3D: u16 = 3;

pub fn kinds() -> Kinds {
    let consts = vec![
        hours(6) as i32,
        days(1) as i32,
        hours(12) as i32,
        days(3) as i32,
    ];
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
        .push_k(K_6H)
        .op(OpCode::Add)
        .push_k(K_1D)
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
        .push_k(K_12H)
        .op(OpCode::Add)
        .push_k(K_3D)
        .op(OpCode::Min)
        .set_need(WATER);
    a.end_rule().bind(next);
    // when light > 128 and water > 1d => sun += 1
    let next = a.label();
    a.sense(Sense::Light).push(128).op(OpCode::Gt).jz(next);
    a.need(WATER).push_k(K_1D).op(OpCode::Gt).jz(next);
    a.mem(0).push(1).op(OpCode::Add).set_mem(0);
    a.end_rule().bind(next);
    // when sun >= 40 and nearest free within 2 as c => { sun = 0; spawn seed at c }
    let next = a.label();
    a.mem(0).push(40).op(OpCode::Ge).jz(next);
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
            tags: 0,
            cadence_shift: CADENCE_SHIFT,
            sight: 2,
            fuel: 512,
            food: hours(1) as i32 / 2,
            bite: 1,
            needs: vec![
                need("water", days(1) as i32, true),
                need("health", 1, false),
            ],
            mems: vec!["lit".into()],
            states: 1,
            entry: seed_entry,
        },
        KindDef {
            id: TREE,
            name: "tree".into(),
            glyph: b'T',
            tags: 0,
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
        },
    ];
    Kinds::from_parts(defs, code, consts, vec![])
}
