//! Field laws (`docs/WORLD.md` §3): a field's own rule, run in every cell
//! of a chunk at the field's cadence. The compiler turns a law into a
//! column program: each op runs over a whole chunk at once (one dispatch per
//! op per chunk), which `benches/field_law.rs` measured at native speed once
//! a law reads its neighbours.
//!
//! Columns are the chunk plus a one-cell border (66 x 66), so an aggregate
//! over the 3 x 3 square reads its neighbours in the next chunk; the border
//! holds that chunk's cells, or the chunk's own edge where nothing is loaded
//! (a mirror: diffusion neither leaks nor gains there), or the field's
//! `outside` value. A law computes every cell from the values all fields
//! held before any law ran this tick, so the order of chunks never matters.

use crate::rng::{hash_cell, splitmix64};
use crate::rules::FieldDef;
use crate::stage::{CHUNK_CELLS, CHUNK_SIZE, ChunkCells, ChunkCoord};

/// Cells a side of a column: the chunk and a border of one.
pub const SIDE: usize = CHUNK_SIZE as usize + 2;
/// Cells in a column.
pub const COLUMN: usize = SIDE * SIDE;
/// Ops (and so columns) in one law.
pub const MAX_LAW_OPS: usize = 128;
/// Hash stream of a law's dice; the field, the draw and the tick mix in.
pub const STREAM_LAW: u64 = 0x0009;

/// Two columns in, one out.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Bin {
    Add,
    Sub,
    Mul,
    /// `x / 0` is 0, as in the rules.
    Div,
    Mod,
    Lt,
    Le,
    Eq,
    Ne,
    Ge,
    Gt,
    Min,
    Max,
    /// Both nonzero: 1, else 0.
    And,
    Or,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Un {
    Neg,
    Abs,
    Sign,
    /// Zero: 1, else 0.
    Not,
}

/// An aggregate over the 3 x 3 square, the cell itself included.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Agg {
    Sum,
    Min,
    Max,
}

/// One op: it writes the column of its own index (`ops[i]` writes column
/// `i`), from columns before it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LawOp {
    /// Field `f`, border included.
    Field(u8),
    Const(i32),
    /// The clock: `0` hour, `1` day, `2` light. One value a tick.
    Clock(u8),
    Bin(Bin, u16, u16),
    Un(Un, u16),
    /// `clamp(x, lo, hi)`.
    Clamp(u16, u16, u16),
    /// `m != 0 ? a : b`.
    Select(u16, u16, u16),
    /// `rand(n)`: 0 to n - 1 (0 for n <= 0), draw number `.1`.
    Rand(u16, u16),
    /// `chance(p)`: 1 with p in 100, draw number `.1`.
    Chance(u16, u16),
    /// Over the 3 x 3 square of a column; the border of the result is 0 (no
    /// op reads it: an aggregate never holds another).
    Agg(Agg, u16),
}

/// A field's law: its cadence and its program, whose column `result` is the
/// field's new value (clamped to `0..=max` when it is stored).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Law {
    /// Ticks between two runs in a chunk (staggered by the chunk).
    pub every: u32,
    pub ops: Vec<LawOp>,
    pub result: u16,
}

impl Law {
    /// Every value of the law as numbers, for the rules hash.
    pub fn words(&self) -> impl Iterator<Item = u64> + '_ {
        let op = |o: &LawOp| -> [u64; 4] {
            match *o {
                LawOp::Field(f) => [1, u64::from(f), 0, 0],
                LawOp::Const(v) => [2, u64::from(v as u32), 0, 0],
                LawOp::Clock(c) => [3, u64::from(c), 0, 0],
                LawOp::Bin(b, x, y) => [4, b as u64, u64::from(x), u64::from(y)],
                LawOp::Un(u, x) => [5, u as u64, u64::from(x), 0],
                LawOp::Clamp(x, l, h) => [6, u64::from(x), u64::from(l), u64::from(h)],
                LawOp::Select(m, a, b) => [7, u64::from(m), u64::from(a), u64::from(b)],
                LawOp::Rand(n, d) => [8, u64::from(n), u64::from(d), 0],
                LawOp::Chance(p, d) => [9, u64::from(p), u64::from(d), 0],
                LawOp::Agg(a, x) => [10, a as u64, u64::from(x), 0],
            }
        };
        [u64::from(self.every), u64::from(self.result)]
            .into_iter()
            .chain(self.ops.iter().flat_map(op))
    }
}

/// Is a law of cadence `every` due in chunk `c` at `tick`? Staggered by a
/// hash of the coordinate, as scent and actors are.
#[inline]
pub fn law_due(tick: u64, c: ChunkCoord, every: u32) -> bool {
    let stagger = splitmix64(u64::from(c.x as u32) << 32 | u64::from(c.y as u32) ^ 0x1A3);
    tick.wrapping_add(stagger)
        .is_multiple_of(u64::from(every.max(1)))
}

/// A field's layers around a chunk: the chunk's own, then its eight
/// neighbours' (`None`: not loaded), in row order from the top-left.
pub type Around<'a> = [Option<&'a [u16; CHUNK_CELLS]>; 9];

/// One field's column, border included: the neighbours' cells where they
/// are loaded, else `outside`, else the chunk's own nearest edge cell.
pub fn fill(col: &mut [i32], around: &Around<'_>, outside: Option<u16>) {
    let own = around[4].expect("the chunk's own layer");
    let n = CHUNK_SIZE as usize;
    for y in 0..SIDE {
        for x in 0..SIDE {
            // Which chunk of the 3 x 3, and the cell in it.
            let (cx, lx) = side(x, n);
            let (cy, ly) = side(y, n);
            let v = match around[cy * 3 + cx] {
                Some(layer) => layer[ly * n + lx],
                None => outside.unwrap_or_else(|| {
                    // The own chunk's edge cell nearest to it.
                    let ex = if cx == 1 {
                        lx
                    } else if cx == 0 {
                        0
                    } else {
                        n - 1
                    };
                    let ey = if cy == 1 {
                        ly
                    } else if cy == 0 {
                        0
                    } else {
                        n - 1
                    };
                    own[ey * n + ex]
                }),
            };
            col[y * SIDE + x] = i32::from(v);
        }
    }
}

/// Column index `i` (0..SIDE) as (chunk 0..3, cell in it).
#[inline]
fn side(i: usize, n: usize) -> (usize, usize) {
    match i {
        0 => (0, n - 1),
        i if i <= n => (1, i - 1),
        _ => (2, 0),
    }
}

/// What a law reads besides its columns.
#[derive(Debug, Clone, Copy)]
pub struct LawCtx<'a> {
    pub seed: u64,
    pub tick: u64,
    pub coord: ChunkCoord,
    /// The field the law belongs to: its dice are its own.
    pub field: u8,
    /// Hour, day, light.
    pub clock: [i32; 3],
    /// The chunk and its eight neighbours, in row order from the top-left
    /// (`None`: not loaded).
    pub chunks: [Option<&'a ChunkCells>; 9],
    /// Every field, for its `outside`.
    pub defs: &'a [FieldDef],
}

/// Run `law` over a chunk into `out` (the inner cells of the result,
/// clamped to `0..=max`). `cols` is scratch, grown to the law's length.
pub fn run(
    law: &Law,
    ctx: &LawCtx<'_>,
    max: u16,
    cols: &mut Vec<i32>,
    out: &mut [u16; CHUNK_CELLS],
) {
    if cols.len() < law.ops.len() * COLUMN {
        cols.resize(law.ops.len() * COLUMN, 0);
    }
    for (i, op) in law.ops.iter().enumerate() {
        let (done, rest) = cols.split_at_mut(i * COLUMN);
        let dst = &mut rest[..COLUMN];
        let c = |j: u16| &done[usize::from(j) * COLUMN..(usize::from(j) + 1) * COLUMN];
        match *op {
            LawOp::Field(f) => {
                let f = usize::from(f);
                let around: Around<'_> =
                    std::array::from_fn(|k| ctx.chunks[k].and_then(|c| c.fields.get(f)));
                fill(dst, &around, ctx.defs[f].outside);
            }
            LawOp::Const(v) => dst.fill(v),
            LawOp::Clock(k) => dst.fill(ctx.clock[usize::from(k)]),
            LawOp::Bin(b, x, y) => {
                let (x, y) = (c(x), c(y));
                for ((d, &a), &b2) in dst.iter_mut().zip(x).zip(y) {
                    *d = bin(b, a, b2);
                }
            }
            LawOp::Un(u, x) => {
                for (d, &a) in dst.iter_mut().zip(c(x)) {
                    *d = match u {
                        Un::Neg => a.wrapping_neg(),
                        Un::Abs => a.wrapping_abs(),
                        Un::Sign => a.signum(),
                        Un::Not => i32::from(a == 0),
                    };
                }
            }
            LawOp::Clamp(x, l, h) => {
                let (x, l, h) = (c(x), c(l), c(h));
                for i in 0..COLUMN {
                    // As the rules' `clamp`: a crossed pair gives the low bound.
                    dst[i] = if l[i] <= h[i] {
                        x[i].clamp(l[i], h[i])
                    } else {
                        l[i]
                    };
                }
            }
            LawOp::Select(m, a, b) => {
                let (m, a, b) = (c(m), c(a), c(b));
                for i in 0..COLUMN {
                    dst[i] = if m[i] != 0 { a[i] } else { b[i] };
                }
            }
            LawOp::Rand(n, draw) | LawOp::Chance(n, draw) => {
                let n = c(n);
                let stream = splitmix64(
                    STREAM_LAW
                        ^ splitmix64(ctx.tick)
                        ^ (u64::from(ctx.field) << 40)
                        ^ (u64::from(draw) << 48),
                );
                let (x0, y0) = (ctx.coord.x * CHUNK_SIZE - 1, ctx.coord.y * CHUNK_SIZE - 1);
                let chance = matches!(op, LawOp::Chance(..));
                for y in 0..SIDE {
                    for x in 0..SIDE {
                        let i = y * SIDE + x;
                        // World coordinates: a cell draws the same in the
                        // border of a neighbour as in its own chunk.
                        #[allow(clippy::cast_possible_wrap)] // SIDE is 66
                        #[allow(clippy::cast_possible_truncation)]
                        // below SIDE, 66
                        let (x, y) = (x as i32, y as i32);
                        let h = hash_cell(ctx.seed ^ stream, stream, x0 + x, y0 + y);
                        dst[i] = if chance {
                            i32::from(h % 100 < n[i].clamp(0, 100) as u64)
                        } else if n[i] <= 0 {
                            0
                        } else {
                            #[allow(clippy::cast_possible_truncation)] // below n, an i32
                            let v = (h % n[i] as u64) as i32;
                            v
                        };
                    }
                }
            }
            LawOp::Agg(a, x) => {
                let x = c(x);
                dst.fill(0);
                for y in 1..SIDE - 1 {
                    for xx in 1..SIDE - 1 {
                        let mut acc = match a {
                            Agg::Sum => 0i32,
                            Agg::Min => i32::MAX,
                            Agg::Max => i32::MIN,
                        };
                        for dy in [y - 1, y, y + 1] {
                            let row = &x[dy * SIDE + xx - 1..dy * SIDE + xx + 2];
                            for &v in row {
                                acc = match a {
                                    Agg::Sum => acc.wrapping_add(v),
                                    Agg::Min => acc.min(v),
                                    Agg::Max => acc.max(v),
                                };
                            }
                        }
                        dst[y * SIDE + xx] = acc;
                    }
                }
            }
        }
    }
    let r = &cols[usize::from(law.result) * COLUMN..(usize::from(law.result) + 1) * COLUMN];
    let n = CHUNK_SIZE as usize;
    for y in 0..n {
        for x in 0..n {
            let v = r[(y + 1) * SIDE + x + 1].clamp(0, i32::from(max));
            out[y * n + x] = u16::try_from(v).expect("clamped into 0..=max");
        }
    }
}

#[inline]
fn bin(b: Bin, x: i32, y: i32) -> i32 {
    match b {
        Bin::Add => x.wrapping_add(y),
        Bin::Sub => x.wrapping_sub(y),
        Bin::Mul => x.wrapping_mul(y),
        Bin::Div if y == 0 => 0,
        Bin::Div => x.wrapping_div(y),
        Bin::Mod if y == 0 => 0,
        Bin::Mod => x.wrapping_rem(y),
        Bin::Lt => i32::from(x < y),
        Bin::Le => i32::from(x <= y),
        Bin::Eq => i32::from(x == y),
        Bin::Ne => i32::from(x != y),
        Bin::Ge => i32::from(x >= y),
        Bin::Gt => i32::from(x > y),
        Bin::Min => x.min(y),
        Bin::Max => x.max(y),
        Bin::And => i32::from(x != 0 && y != 0),
        Bin::Or => i32::from(x != 0 || y != 0),
    }
}
