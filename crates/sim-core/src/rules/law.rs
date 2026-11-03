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

use crate::actors::ChunkActors;
use crate::rng::{hash_cell, splitmix64};
use crate::rules::Kinds;
use crate::rules::vm::Want;
use crate::stage::{CHUNK_CELLS, CHUNK_SIZE, ChunkCells, ChunkCoord, Feature};

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
    /// `flow(w, l, k)`: what flows into a cell from its four neighbours, net.
    /// Each pair moves `(l' - l) / k` (towards zero) from the higher `l` to
    /// the lower, at most a quarter of the giver's `w`; both cells of a pair
    /// work it out alike, so what one gains the other loses. Nothing flows
    /// across the chunk's edge where no chunk is loaded beyond it (that
    /// border is made up and never kept). Its border is 0, as an
    /// aggregate's.
    Flow(u16, u16, i32),
    /// `is(here, pred)`: 1 where the cell matches the predicate (a kind,
    /// a tag, a material, `free`), as an actor's search sees it; where
    /// nothing is loaded, rock with nobody.
    Is(i32),
}

/// A field's law: its cadence and its program, whose column `result` is the
/// field's new value (clamped to `0..=max` when it is stored).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Law {
    /// Ticks between two runs in a chunk (staggered by the chunk).
    pub every: u32,
    pub ops: Vec<LawOp>,
    pub result: u16,
    /// [`Law::leaves_zero`], worked out once by the compiler.
    pub quiet: bool,
    /// The law holds a `flow`: it runs in every chunk at the same tick, not
    /// staggered, so the two cells of a pair across a border move together.
    pub in_step: bool,
}

impl Law {
    /// Does the law leave every cell at 0 where every field it reads is 0?
    /// Then a chunk where they are all 0 (and its neighbours' too, if the
    /// law reads them) can be skipped: the law would write 0 over 0. Known
    /// only for a law without dice or the clock, evaluated once on fields
    /// of 0 (every cell alike, so an aggregate is nine of a cell, or one).
    pub fn leaves_zero(&self) -> bool {
        let mut v: Vec<i32> = Vec::with_capacity(self.ops.len());
        for op in &self.ops {
            let c = |j: u16| v[usize::from(j)];
            let x = match *op {
                LawOp::Field(_) => 0,
                LawOp::Const(k) => k,
                // Actors and dice aren't fields: no knowing.
                LawOp::Clock(_) | LawOp::Rand(..) | LawOp::Chance(..) | LawOp::Is(_) => {
                    return false;
                }
                LawOp::Bin(b, x, y) => bin(b, c(x), c(y)),
                LawOp::Un(u, x) => un(u, c(x)),
                LawOp::Clamp(x, l, h) => clamp(c(x), c(l), c(h)),
                LawOp::Select(m, a, b) => {
                    if c(m) != 0 {
                        c(a)
                    } else {
                        c(b)
                    }
                }
                LawOp::Agg(Agg::Sum, x) => c(x).wrapping_mul(9),
                LawOp::Agg(_, x) => c(x),
                // Every cell alike: nothing flows.
                LawOp::Flow(..) => 0,
            };
            v.push(x);
        }
        v[usize::from(self.result)] <= 0
    }

    /// The fields the law reads, and whether it reads any neighbour.
    pub fn reads(&self) -> (impl Iterator<Item = u8> + '_, bool) {
        let fields = self.ops.iter().filter_map(|o| match *o {
            LawOp::Field(f) => Some(f),
            _ => None,
        });
        let neighbours = self
            .ops
            .iter()
            .any(|o| matches!(o, LawOp::Agg(..) | LawOp::Flow(..)));
        (fields, neighbours)
    }
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
                LawOp::Flow(w, l, k) => [11, u64::from(w), u64::from(l), u64::from(k as u32)],
                LawOp::Is(p) => [12, u64::from(p as u32), 0, 0],
            }
        };
        [u64::from(self.every), u64::from(self.result)]
            .into_iter()
            .chain(self.ops.iter().flat_map(op))
    }
}

/// Is `law` due in chunk `c` at `tick`? Staggered by a hash of the
/// coordinate, as actors are, unless it runs in step.
#[inline]
pub fn law_due(tick: u64, c: ChunkCoord, law: &Law) -> bool {
    let stagger = if law.in_step {
        0
    } else {
        splitmix64(u64::from(c.x as u32) << 32 | u64::from(c.y as u32) ^ 0x1A3)
    };
    tick.wrapping_add(stagger)
        .is_multiple_of(u64::from(law.every.max(1)))
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
    /// Their actors, alike.
    pub actors: [Option<&'a ChunkActors>; 9],
    /// The fields (for their `outside`), tags, families and materials.
    pub kinds: &'a Kinds,
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
                fill(dst, &around, ctx.kinds.fields[f].outside);
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
                    *d = un(u, a);
                }
            }
            LawOp::Clamp(x, l, h) => {
                let (x, l, h) = (c(x), c(l), c(h));
                for i in 0..COLUMN {
                    dst[i] = clamp(x[i], l[i], h[i]);
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
            LawOp::Is(p) => {
                let k = ctx.kinds;
                let want = Want::of(p, &k.family_end);
                let n = CHUNK_SIZE as usize;
                for y in 0..SIDE {
                    let (cy, ly) = side(y, n);
                    for x in 0..SIDE {
                        let (cx, lx) = side(x, n);
                        let i = ly * n + lx;
                        let hit = match (ctx.chunks[cy * 3 + cx], ctx.actors[cy * 3 + cx]) {
                            (Some(cells), Some(rows)) => {
                                want.test(cells, rows, i, &k.tag_bits, &k.materials.table)
                            }
                            // As a search sees it: rock, nobody.
                            _ => want == Want::Feature(i32::from(Feature::Rock.0)),
                        };
                        dst[y * SIDE + x] = i32::from(hit);
                    }
                }
            }
            LawOp::Flow(w, l, k) => {
                let (w, l) = (c(w), c(l));
                dst.fill(0);
                // Next to a chunk not loaded the border is made up (a
                // mirror, or `outside`), and nothing it would gain or lose
                // is ever kept: nothing flows across that edge.
                let loaded = |k: usize| ctx.chunks[k].is_some();
                let (west, east) = (loaded(3), loaded(5));
                let (north, south) = (loaded(1), loaded(7));
                // Each edge between two cells once: what the first takes
                // from the second, given to one and taken from the other
                // where they are inside the chunk. Four edges a cell, each
                // at most a quarter of an i32: the sum fits.
                for y in 1..SIDE - 1 {
                    for x in 0..SIDE - 1 {
                        if (x == 0 && !west) || (x + 1 == SIDE - 1 && !east) {
                            continue;
                        }
                        let (i, j) = (y * SIDE + x, y * SIDE + x + 1);
                        let a = pair(w[i], l[i], w[j], l[j], k);
                        if x >= 1 {
                            dst[i] += a;
                        }
                        if x + 1 < SIDE - 1 {
                            dst[j] -= a;
                        }
                    }
                }
                for y in 0..SIDE - 1 {
                    if (y == 0 && !north) || (y + 1 == SIDE - 1 && !south) {
                        continue;
                    }
                    for x in 1..SIDE - 1 {
                        let (i, j) = (y * SIDE + x, (y + 1) * SIDE + x);
                        let a = pair(w[i], l[i], w[j], l[j], k);
                        if y >= 1 {
                            dst[i] += a;
                        }
                        if y + 1 < SIDE - 1 {
                            dst[j] -= a;
                        }
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

/// What flows into a cell (`w`, `l`) from its neighbour (`w2`, `l2`):
/// `(l2 - l) / k` towards zero, from the higher to the lower, at most a
/// quarter of what the giver holds. `pair(a, b) == -pair(b, a)`, and it is
/// never more than a quarter of an i32 either way.
#[inline]
pub fn pair(w: i32, l: i32, w2: i32, l2: i32, k: i32) -> i32 {
    let k = k.max(1);
    let d = match l2.checked_sub(l) {
        // `-i32::MIN` doesn't fit: one short, as the other side clamps.
        Some(d) => (d / k).max(-i32::MAX),
        // Wider than an i32, but no wider once divided by 2 or more.
        #[allow(clippy::cast_possible_truncation)]
        None => ((i64::from(l2) - i64::from(l)) / i64::from(k))
            .clamp(-i64::from(i32::MAX), i64::from(i32::MAX)) as i32,
    };
    if d > 0 {
        d.min(w2.max(0) / 4)
    } else {
        -(-d).min(w.max(0) / 4)
    }
}

#[inline]
fn un(u: Un, a: i32) -> i32 {
    match u {
        Un::Neg => a.wrapping_neg(),
        Un::Abs => a.wrapping_abs(),
        Un::Sign => a.signum(),
        Un::Not => i32::from(a == 0),
    }
}

/// As the rules' `clamp`: a crossed pair gives the low bound.
#[inline]
fn clamp(x: i32, lo: i32, hi: i32) -> i32 {
    if lo <= hi { x.clamp(lo, hi) } else { lo }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rng::splitmix64;

    /// What one cell of a pair gains the other loses, and no cell gives
    /// more than it holds to its four neighbours.
    #[test]
    fn a_pair_moves_one_amount_and_a_giver_keeps_what_it_gives() {
        let mut h = 7u64;
        let mut draw = |m: i64| {
            h = splitmix64(h);
            #[allow(clippy::cast_possible_truncation, clippy::cast_possible_wrap)]
            let v = (h % (2 * m as u64 + 1)) as i64 - m;
            i32::try_from(v).expect("within m")
        };
        for _ in 0..10_000 {
            let (w, l, w2, l2) = (
                draw(70_000),
                draw(i32::MAX as i64),
                draw(70_000),
                draw(i32::MAX as i64),
            );
            let k = draw(40).abs() + 1;
            assert_eq!(
                pair(w, l, w2, l2, k),
                -pair(w2, l2, w, l, k),
                "{w} {l} {w2} {l2} {k}"
            );
            let out = -pair(w, l, w2, l2, k);
            assert!(out <= w.max(0) / 4, "gives at most a quarter: {out} of {w}");
        }
        let ends = [i32::MIN, i32::MIN + 1, -1, 0, 1, i32::MAX - 1, i32::MAX];
        for (&w, &l, &w2, &l2) in ends
            .iter()
            .flat_map(|a| ends.iter().map(move |b| (a, b)))
            .flat_map(|(a, b)| ends.iter().map(move |c| (a, b, c)))
            .flat_map(|(a, b, c)| ends.iter().map(move |d| (a, b, c, d)))
        {
            for k in [1, 2, 3, i32::MAX] {
                assert_eq!(
                    pair(w, l, w2, l2, k),
                    -pair(w2, l2, w, l, k),
                    "{w} {l} {w2} {l2} {k}"
                );
            }
        }
    }
}
