//! `cargo bench -p sim-core --bench field_law`
//! What a per-cell law costs (docs/WORLD.md, "Cost"): three laws over 100
//! chunks, each run three ways. `native` is a hand-written loop (the ceiling),
//! `per cell` a checked stack interpreter run once per cell (how the rules VM
//! runs a think), `per chunk` the same bytecode with each op run over a whole
//! chunk's column (one dispatch per op per chunk). All three give the same
//! cells; `check` asserts it before timing. Standalone: nothing here is engine
//! code yet, the bench decides which form the engine gets.
use criterion::{Criterion, Throughput, criterion_group, criterion_main};
use std::hint::black_box;

const SIDE: usize = 64;
const CELLS: usize = SIDE * SIDE;
const CHUNKS: usize = 100;

/// A chunk's fields, a byte a cell, like scent today.
#[derive(Clone)]
struct Chunk {
    fields: Vec<[u8; CELLS]>,
}

#[derive(Clone, Copy)]
enum Op {
    /// `-> field[a]` here
    Load(usize),
    /// `-> the sum of field[a] over the 3x3 square` (edges repeat)
    Sum3(usize),
    Push(i32),
    Add,
    Div,
    Min,
    Max,
    Mul,
    Eq,
    /// `x ->`; `next field[a] = clamp(x, 0, 255)`
    Store(usize),
}

struct Law {
    name: &'static str,
    fields: usize,
    code: Vec<Op>,
    native: fn(&Chunk, &mut Chunk),
}

/// The 3x3 sum around (x, y): nine cells, an edge cell repeated where the
/// square leaves the chunk. Every form uses this one, so the bench measures
/// dispatch, not neighbour reads.
#[inline]
fn sum3(f: &[u8; CELLS], x: usize, y: usize) -> i32 {
    let rows = [y.saturating_sub(1), y, (y + 1).min(SIDE - 1)];
    let cols = [x.saturating_sub(1), x, (x + 1).min(SIDE - 1)];
    let mut s = 0;
    for yy in rows {
        for xx in cols {
            s += i32::from(f[yy * SIDE + xx]);
        }
    }
    s
}

#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // clamped to 0..=255
fn byte(v: i32) -> u8 {
    v.clamp(0, 255) as u8
}

// ---- native: the laws by hand ----

fn regrow_native(prev: &Chunk, next: &mut Chunk) {
    let (s, cap) = (&prev.fields[0], &prev.fields[1]);
    for i in 0..CELLS {
        next.fields[0][i] = s[i].saturating_add(1).min(cap[i]);
    }
}

fn diffuse_native(prev: &Chunk, next: &mut Chunk) {
    let h = &prev.fields[0];
    for y in 0..SIDE {
        for x in 0..SIDE {
            next.fields[0][y * SIDE + x] = byte(sum3(h, x, y) / 9);
        }
    }
}

fn life_native(prev: &Chunk, next: &mut Chunk) {
    let a = &prev.fields[0];
    for y in 0..SIDE {
        for x in 0..SIDE {
            let i = y * SIDE + x;
            let n = sum3(a, x, y);
            next.fields[0][i] = u8::from(n == 3 || (a[i] == 1 && n == 4));
        }
    }
}

// ---- per cell: a checked stack interpreter, as the VM runs a think ----

#[derive(Debug)]
enum Trap {
    Stack,
    Fuel,
}

struct Cell<'a> {
    prev: &'a Chunk,
    x: usize,
    y: usize,
    stack: [i32; 16],
    sp: usize,
    fuel: u32,
}

impl Cell<'_> {
    #[inline]
    fn push(&mut self, v: i32) -> Result<(), Trap> {
        *self.stack.get_mut(self.sp).ok_or(Trap::Stack)? = v;
        self.sp += 1;
        Ok(())
    }

    #[inline]
    fn pop(&mut self) -> Result<i32, Trap> {
        self.sp = self.sp.checked_sub(1).ok_or(Trap::Stack)?;
        Ok(self.stack[self.sp])
    }

    fn run(&mut self, code: &[Op], next: &mut Chunk) -> Result<(), Trap> {
        let i = self.y * SIDE + self.x;
        for &op in code {
            self.fuel = self.fuel.checked_sub(1).ok_or(Trap::Fuel)?;
            match op {
                Op::Load(f) => self.push(i32::from(self.prev.fields[f][i]))?,
                Op::Sum3(f) => self.push(sum3(&self.prev.fields[f], self.x, self.y))?,
                Op::Push(k) => self.push(k)?,
                Op::Store(f) => next.fields[f][i] = byte(self.pop()?),
                _ => {
                    let b = self.pop()?;
                    let a = self.pop()?;
                    self.push(bin(op, a, b))?;
                }
            }
        }
        Ok(())
    }
}

#[inline]
fn bin(op: Op, a: i32, b: i32) -> i32 {
    match op {
        Op::Add => a.wrapping_add(b),
        Op::Div => a.checked_div(b).unwrap_or(0),
        Op::Min => a.min(b),
        Op::Max => a.max(b),
        Op::Mul => a.wrapping_mul(b),
        Op::Eq => i32::from(a == b),
        _ => unreachable!(),
    }
}

fn per_cell(code: &[Op], prev: &Chunk, next: &mut Chunk) {
    for y in 0..SIDE {
        for x in 0..SIDE {
            let mut c = Cell {
                prev,
                x,
                y,
                stack: [0; 16],
                sp: 0,
                fuel: 4096,
            };
            c.run(code, next).unwrap();
        }
    }
}

// ---- per chunk: the same bytecode, each op over a whole column ----

/// A stack of columns, allocated once and reused for every chunk.
struct Columns {
    cols: Vec<[i32; CELLS]>,
}

impl Columns {
    fn new() -> Self {
        Self {
            cols: vec![[0; CELLS]; 8],
        }
    }

    fn run(&mut self, code: &[Op], prev: &Chunk, next: &mut Chunk) {
        let mut sp = 0;
        for &op in code {
            match op {
                Op::Load(f) => {
                    for (d, &s) in self.cols[sp].iter_mut().zip(&prev.fields[f]) {
                        *d = i32::from(s);
                    }
                    sp += 1;
                }
                Op::Sum3(f) => {
                    let col = &mut self.cols[sp];
                    for y in 0..SIDE {
                        for x in 0..SIDE {
                            col[y * SIDE + x] = sum3(&prev.fields[f], x, y);
                        }
                    }
                    sp += 1;
                }
                Op::Push(k) => {
                    self.cols[sp] = [k; CELLS];
                    sp += 1;
                }
                Op::Store(f) => {
                    sp -= 1;
                    for (d, &s) in next.fields[f].iter_mut().zip(&self.cols[sp]) {
                        *d = byte(s);
                    }
                }
                _ => {
                    sp -= 1;
                    let (lo, hi) = self.cols.split_at_mut(sp);
                    let (a, b) = (&mut lo[sp - 1], &hi[0]);
                    match op {
                        Op::Add => a
                            .iter_mut()
                            .zip(b)
                            .for_each(|(a, &b)| *a = a.wrapping_add(b)),
                        Op::Div => a
                            .iter_mut()
                            .zip(b)
                            .for_each(|(a, &b)| *a = a.checked_div(b).unwrap_or(0)),
                        Op::Min => a.iter_mut().zip(b).for_each(|(a, &b)| *a = (*a).min(b)),
                        Op::Max => a.iter_mut().zip(b).for_each(|(a, &b)| *a = (*a).max(b)),
                        Op::Mul => a
                            .iter_mut()
                            .zip(b)
                            .for_each(|(a, &b)| *a = a.wrapping_mul(b)),
                        Op::Eq => a
                            .iter_mut()
                            .zip(b)
                            .for_each(|(a, &b)| *a = i32::from(*a == b)),
                        _ => unreachable!(),
                    }
                }
            }
        }
    }
}

// ---- the laws and their worlds ----

fn laws() -> Vec<Law> {
    use Op::*;
    vec![
        // sugar = min(sugar + 1, cap)
        Law {
            name: "regrow",
            fields: 2,
            code: vec![Load(0), Push(1), Add, Load(1), Min, Store(0)],
            native: regrow_native,
        },
        // heat = avg heat within 1
        Law {
            name: "diffuse",
            fields: 1,
            code: vec![Sum3(0), Push(9), Div, Store(0)],
            native: diffuse_native,
        },
        // alive = (n == 3) or (alive and n == 4), n counting the cell itself
        Law {
            name: "life",
            fields: 1,
            code: vec![
                Sum3(0),
                Push(3),
                Eq,
                Load(0),
                Sum3(0),
                Push(4),
                Eq,
                Mul,
                Max,
                Store(0),
            ],
            native: life_native,
        },
    ]
}

/// `CHUNKS` chunks of hashed bytes: field 0 is 0 or 1 for Life, else 0..=255.
fn world(law: &Law) -> Vec<Chunk> {
    let mut h: u64 = 0x9E37_79B9_7F4A_7C15;
    let mut next = || {
        h ^= h << 13;
        h ^= h >> 7;
        h ^= h << 17;
        h
    };
    (0..CHUNKS)
        .map(|_| Chunk {
            fields: (0..law.fields)
                .map(|_| {
                    std::array::from_fn(|_| {
                        let v = (next() >> 56) as u8;
                        if law.name == "life" { v & 1 } else { v }
                    })
                })
                .collect(),
        })
        .collect()
}

/// The three forms agree on every cell.
fn check(law: &Law, prev: &[Chunk]) {
    let mut cols = Columns::new();
    for p in prev {
        let (mut a, mut b, mut c) = (p.clone(), p.clone(), p.clone());
        (law.native)(p, &mut a);
        per_cell(&law.code, p, &mut b);
        cols.run(&law.code, p, &mut c);
        assert!(a.fields == b.fields && a.fields == c.fields, "{}", law.name);
    }
}

fn bench_laws(c: &mut Criterion) {
    let mut g = c.benchmark_group("field_law");
    g.throughput(Throughput::Elements((CHUNKS * CELLS) as u64));
    for law in laws() {
        let prev = world(&law);
        check(&law, &prev);
        let mut next = prev.clone();
        g.bench_function(format!("{}/native", law.name), |b| {
            b.iter(|| {
                for (p, n) in prev.iter().zip(&mut next) {
                    (law.native)(black_box(p), n);
                }
            });
        });
        g.bench_function(format!("{}/per cell", law.name), |b| {
            b.iter(|| {
                for (p, n) in prev.iter().zip(&mut next) {
                    per_cell(black_box(&law.code), black_box(p), n);
                }
            });
        });
        let mut cols = Columns::new();
        g.bench_function(format!("{}/per chunk", law.name), |b| {
            b.iter(|| {
                for (p, n) in prev.iter().zip(&mut next) {
                    cols.run(black_box(&law.code), black_box(p), n);
                }
            });
        });
    }
    g.finish();
}

criterion_group!(benches, bench_laws);
criterion_main!(benches);
