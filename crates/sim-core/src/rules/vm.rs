//! The rules virtual machine: a fuel-bounded integer stack machine that runs
//! one actor's think (`docs/ACTORS.md` §5).
//!
//! A program is a flat `Vec<Op>` shared read-only by every thread. A think
//! runs a kind's entry point from the top with an empty stack; the only state
//! that persists is the actor's row (needs, mem, state). The machine can
//! read the tick-start world through a [`Halo`] (own chunk and the eight
//! around it), read and write its own row, draw from a counter-based RNG,
//! and emit at most one [`Action`] plus a `next` state. Every op costs one
//! unit of fuel, a search costs the square it scans; fuel out, a bad jump, a
//! stack fault or a second action end the think with `idle` and a
//! [`Trap`]. The VM never panics on a program.
//!
//! Rule structure is compiled to jumps: `cond; Jz next; body; EndRule`.
//! `EndRule` halts if the body emitted an action or a `next`, otherwise
//! execution falls through to the next rule. Falling off the end is `idle`.

use crate::actors::ChunkActors;
use crate::actors::{ActorMind, MEM_SLOTS, NEED_SLOTS};
use crate::rng::splitmix64;
use crate::stage::{ActorId, CHUNK_BITS, CHUNK_SIZE, ChunkCells, Feature, Ground, Pos};
use crate::time::{Clock, daylight};

use super::KindDef;

/// One instruction: opcode, a small operand, a 16-bit immediate. Larger
/// constants go through the constant pool (`PushK`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Op {
    pub code: OpCode,
    pub a: u8,
    pub imm: i16,
}

impl Op {
    pub const fn new(code: OpCode, a: u8, imm: i16) -> Self {
        Self { code, a, imm }
    }

    /// The instruction as 32 bits, for hashing a program.
    pub fn bits(self) -> u32 {
        u32::from(self.code as u8) | u32::from(self.a) << 8 | u32::from(self.imm as u16) << 16
    }
}

/// The instruction set. Stack effects in comments as `pops -> pushes`.
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum OpCode {
    /// `-> imm`
    Push,
    /// `-> consts[imm]`
    PushK,
    /// `x ->`
    Pop,
    /// `-> locals[a]` (relative to the current frame)
    Load,
    /// `x ->`; `locals[a] = x`
    Store,
    /// `-> needs[a]`
    Need,
    /// `x ->`; `needs[a] = clamp(x, 0, max)`
    SetNeed,
    /// `-> mem[a]`
    Mem,
    /// `x ->`; `mem[a] = x`
    SetMem,
    /// `-> sense a` (see [`Sense`])
    Sense,
    Add,
    Sub,
    Mul,
    /// `x y -> x / y`, `0` when `y == 0`
    Div,
    /// `x y -> x % y`, `0` when `y == 0`
    Mod,
    Neg,
    /// `x -> !x` (1 if zero)
    Not,
    Lt,
    Le,
    Eq,
    Ne,
    Ge,
    Gt,
    /// `x y -> x && y` (both nonzero)
    And,
    Or,
    Min,
    Max,
    Abs,
    Sign,
    /// `x lo hi -> clamp(x, lo, hi)`
    Clamp,
    /// `pc += imm`
    Jmp,
    /// `x ->`; `pc += imm` if `x == 0`
    Jz,
    /// `x ->`; `pc += imm` if `x != 0`
    Jnz,
    /// Call `subs[imm]` with `a` arguments: the top `a` stack values become
    /// the callee's first locals (in order).
    Call,
    /// Return; `a == 1` returns the top of stack to the caller.
    Ret,
    /// `n -> rand in 0..n` (0 when `n <= 0`)
    Rand,
    /// `p -> 1 with probability p percent`
    Chance,
    /// `pred r -> count` of matching cells within Chebyshev `r` (own cell included)
    Count,
    /// `pred r -> found`; on success `locals[a], locals[a+1] = dx, dy` of the
    /// nearest matching cell (rings 1..=r, start rotated by one draw)
    Nearest,
    /// `dx dy -> max(|dx|, |dy|)`
    Dist,
    /// `dx dy -> 1` if the cell there is walkable and empty at tick start
    FreeAt,
    /// `dx dy pred -> 1` if the cell there matches `pred`
    IsAt,
    /// `i -> dx dy` of direction `i` (1..=8 clockwise from north, as
    /// `hurt_dir` reports it); anything else is `0 0`
    DirOf,
    /// `v ->`; set own `look` byte (an effect: it rides along with the
    /// action and does not end the rule)
    SetLook,
    /// Emit action `a` (see [`Action`]); pops its operands
    Act,
    /// `next a`: switch to state `a` for the following think
    Next,
    /// Halt if an action or a `next` was emitted, else fall through
    EndRule,
    Halt,
}

/// Senses readable with `Sense a`.
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sense {
    /// `time::daylight(tick)`, 0..=255
    Light,
    /// Ticks since `born`
    Age,
    X,
    Y,
    /// 0..24
    Hour,
    Day,
    Kind,
    Look,
    Signal,
    State,
    Hurt,
    HurtDir,
    /// Result of the last action (see [`Result`])
    Result,
    /// Own cell's ground, as a predicate value
    Ground,
    /// Own cell's feature, as a predicate value
    Feature,
}

impl Sense {
    pub const COUNT: u8 = 15;

    pub fn from_u8(a: u8) -> Option<Self> {
        (a < Self::COUNT).then(|| {
            // SAFETY-free: the enum is `repr(u8)` and dense from 0.
            [
                Self::Light,
                Self::Age,
                Self::X,
                Self::Y,
                Self::Hour,
                Self::Day,
                Self::Kind,
                Self::Look,
                Self::Signal,
                Self::State,
                Self::Hurt,
                Self::HurtDir,
                Self::Result,
                Self::Ground,
                Self::Feature,
            ][usize::from(a)]
        })
    }
}

/// Actions an actor can emit, one per think. Operands are popped in the
/// order listed.
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Action {
    #[default]
    Idle,
    Die,
    /// `kind`
    Become,
    /// `kind dx dy`
    Spawn,
    /// `dx dy`: one step in that direction (Think reduces it to a unit step
    /// and slides around a blocked cell)
    Move,
    /// `dx dy`: refill `water` from the water cell there
    Drink,
    /// `dx dy`: bite the adjacent actor there (`bite` off its `health`);
    /// if it dies this tick, the lowest-key eater gains its kind's `food`
    Eat,
    /// `dx dy`: bite the adjacent actor there, no food
    Hit,
}

impl Action {
    pub fn from_u8(a: u8) -> Option<Self> {
        match a {
            0 => Some(Self::Idle),
            1 => Some(Self::Die),
            2 => Some(Self::Become),
            3 => Some(Self::Spawn),
            4 => Some(Self::Move),
            5 => Some(Self::Drink),
            6 => Some(Self::Eat),
            7 => Some(Self::Hit),
            _ => None,
        }
    }
}

/// Result codes of an action, latched in `ActorMind::events` low bits.
pub mod result {
    pub const MASK: u8 = 0b111;
    pub const NONE: u8 = 0;
    pub const OK: u8 = 1;
    pub const BLOCKED: u8 = 2;
    pub const MISSED: u8 = 3;
    pub const REFUSED: u8 = 4;
}

/// Event bits of `ActorMind::events` above the result code.
pub mod event {
    /// The last think ran out of fuel or trapped.
    pub const FUEL: u8 = 1 << 3;
    /// Something was taken from this actor.
    pub const TAKEN: u8 = 1 << 4;
}

/// Why a think ended early. Never fatal: the think becomes `idle`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Trap {
    Fuel,
    BadPc,
    StackOverflow,
    StackUnderflow,
    BadLocal,
    BadNeed,
    BadMem,
    BadSense,
    BadAction,
    BadConst,
    BadSub,
    CallDepth,
    SecondAction,
}

/// What a think decided.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Outcome {
    pub action: Action,
    /// Operand kind for `Become` / `Spawn`.
    pub kind: u16,
    /// Operand offset for `Spawn`.
    pub dx: i8,
    pub dy: i8,
    pub next: Option<u8>,
    /// `look = v` effect, if the think set one.
    pub look: Option<u8>,
    pub trap: Option<Trap>,
    /// Ops executed, for `wmc why` and the fuel counters.
    pub used: u32,
}

// ---- predicates ------------------------------------------------------------------------

/// Predicate values, one `i32` at run time: a kind (`0..TAG_BASE`), a tag
/// (`TAG_BASE + tag index`, matched against the occupant kind's tag bits),
/// or one of these.
pub mod pred {
    pub const FREE: i32 = -1;
    pub const GROUND_BASE: i32 = -0x100;
    pub const FEATURE_BASE: i32 = -0x200;
    pub const TAG_BASE: i32 = 0x1_0000;

    pub const fn ground(g: u8) -> i32 {
        GROUND_BASE - g as i32
    }
    pub const fn feature(f: u8) -> i32 {
        FEATURE_BASE - f as i32
    }
}

/// Does local cell `i` of `cells` match `pred`? `tags` is the tag bitset
/// per kind (`Kinds::tag_bits`).
#[inline]
fn matches(pred: i32, cells: &ChunkCells, i: usize, tags: &[u64]) -> bool {
    if pred >= pred::TAG_BASE {
        let bit = (pred - pred::TAG_BASE) as u32;
        return match cells.occupant[i].unpack() {
            Some((kind, _)) => {
                bit < 64
                    && tags
                        .get(usize::from(kind))
                        .is_some_and(|t| t >> bit & 1 == 1)
            }
            None => false,
        };
    }
    if pred >= 0 {
        return match cells.occupant[i].unpack() {
            Some((kind, _)) => i32::from(kind) == pred,
            None => false,
        };
    }
    if pred == pred::FREE {
        return cells.walkable(i) && cells.occupant[i].is_none();
    }
    if pred > pred::FEATURE_BASE {
        return cells.ground[i] as i32 == pred::GROUND_BASE - pred;
    }
    cells.feature[i] as i32 == pred::FEATURE_BASE - pred
}

// ---- the halo --------------------------------------------------------------------------

/// A chunk and its eight neighbours, as loaded at tick start. Index
/// `(oy + 1) * 3 + (ox + 1)`; `None` = not loaded (a wall: rock, nobody).
/// `tags` is the kind table's tag bitset per kind, for tag predicates.
#[derive(Debug, Clone, Copy)]
pub struct Halo<'a> {
    pub chunks: [Option<(&'a ChunkCells, &'a ChunkActors)>; 9],
    pub tags: &'a [u64],
}

impl<'a> Halo<'a> {
    /// The chunk and local index at `(lx + dx, ly + dy)` from the centre
    /// chunk's local cell `(lx, ly)`. `None` if it falls outside the halo
    /// or the chunk there is not loaded.
    #[inline]
    pub fn at(
        &self,
        lx: i32,
        ly: i32,
        dx: i32,
        dy: i32,
    ) -> Option<(&'a ChunkCells, &'a ChunkActors, usize)> {
        let (x, y) = (lx + dx, ly + dy);
        let ox = x >> CHUNK_BITS;
        let oy = y >> CHUNK_BITS;
        if !(-1..=1).contains(&ox) || !(-1..=1).contains(&oy) {
            return None;
        }
        let idx = ((oy + 1) * 3 + (ox + 1)) as usize;
        let (cells, actors) = self.chunks[idx]?;
        let local = ((y & (CHUNK_SIZE - 1)) * CHUNK_SIZE + (x & (CHUNK_SIZE - 1))) as usize;
        Some((cells, actors, local))
    }

    #[inline]
    pub fn matches(&self, lx: i32, ly: i32, dx: i32, dy: i32, pred: i32) -> bool {
        match self.at(lx, ly, dx, dy) {
            Some((cells, _, i)) => matches(pred, cells, i, self.tags),
            // Unloaded: rock, nobody.
            None => pred == pred::feature(Feature::Rock as u8),
        }
    }

    /// Walkable and empty at tick start.
    #[inline]
    pub fn free(&self, lx: i32, ly: i32, dx: i32, dy: i32) -> bool {
        self.matches(lx, ly, dx, dy, pred::FREE)
    }
}

// ---- the machine -----------------------------------------------------------------------

/// Everything a think may read besides its own row.
#[derive(Debug, Clone, Copy)]
pub struct Ctx<'a> {
    pub halo: &'a Halo<'a>,
    pub kind: &'a KindDef,
    /// Own local cell.
    pub cell: usize,
    /// Own world position.
    pub pos: Pos,
    pub tick: u64,
    /// RNG stream base for this actor at this tick (`draw` counts up from it).
    pub rng: u64,
    /// Own public bytes.
    pub look: u8,
    pub signal: i16,
}

const STACK: usize = 64;
const LOCALS: usize = 64;
const FRAMES: usize = 8;

/// Locals per call frame (a sub's arguments and `let`s).
pub const FRAME_LOCALS: usize = 16;

/// Per-op fuel; a search costs `cells / SEARCH_DIV` extra.
const SEARCH_DIV: u32 = 8;

struct Machine<'m> {
    stack: [i32; STACK],
    sp: usize,
    locals: [i32; LOCALS],
    base: usize,
    frames: [(usize, usize); FRAMES],
    depth: usize,
    pc: usize,
    fuel: u32,
    draws: u64,
    out: Outcome,
    /// An action was emitted (an explicit `idle` counts: it ends the rule).
    acted: bool,
    ctx: Ctx<'m>,
    mind: &'m mut ActorMind,
}

impl Machine<'_> {
    #[inline]
    fn push(&mut self, v: i32) -> Result<(), Trap> {
        if self.sp == STACK {
            return Err(Trap::StackOverflow);
        }
        self.stack[self.sp] = v;
        self.sp += 1;
        Ok(())
    }

    #[inline]
    fn pop(&mut self) -> Result<i32, Trap> {
        if self.sp == 0 {
            return Err(Trap::StackUnderflow);
        }
        self.sp -= 1;
        Ok(self.stack[self.sp])
    }

    #[inline]
    fn local(&self, a: u8) -> Result<usize, Trap> {
        let i = self.base + usize::from(a);
        if usize::from(a) >= FRAME_LOCALS || i >= LOCALS {
            return Err(Trap::BadLocal);
        }
        Ok(i)
    }

    #[inline]
    fn draw(&mut self) -> u64 {
        let v = splitmix64(self.ctx.rng.wrapping_add(self.draws));
        self.draws += 1;
        v
    }

    #[inline]
    fn spend(&mut self, n: u32) -> Result<(), Trap> {
        if self.fuel < n {
            self.fuel = 0;
            return Err(Trap::Fuel);
        }
        self.fuel -= n;
        Ok(())
    }

    fn sense(&self, s: Sense) -> i32 {
        let c = &self.ctx;
        match s {
            Sense::Light => i32::from(daylight(c.tick)),
            Sense::Age => (c.tick as u32).wrapping_sub(self.mind.born) as i32,
            Sense::X => c.pos.x,
            Sense::Y => c.pos.y,
            Sense::Hour => i32::from(Clock::at(c.tick).hour),
            Sense::Day => Clock::at(c.tick).day as i32,
            Sense::Kind => i32::from(c.kind.id),
            Sense::Look => i32::from(c.look),
            Sense::Signal => i32::from(c.signal),
            Sense::State => i32::from(self.mind.state),
            Sense::Hurt => i32::from(self.mind.hurt),
            Sense::HurtDir => i32::from(self.mind.hurt_dir),
            Sense::Result => i32::from(self.mind.events & result::MASK),
            Sense::Ground => {
                let (cells, _, i) = c.halo.at(lx(c.cell), ly(c.cell), 0, 0).expect("own chunk");
                pred::ground(cells.ground[i] as u8)
            }
            Sense::Feature => {
                let (cells, _, i) = c.halo.at(lx(c.cell), ly(c.cell), 0, 0).expect("own chunk");
                pred::feature(cells.feature[i] as u8)
            }
        }
    }

    /// Cells within Chebyshev `r` of self matching `pred`, self included.
    fn count(&mut self, pred: i32, r: i32) -> Result<i32, Trap> {
        let r = r.clamp(0, i32::from(self.ctx.kind.sight));
        let side = (2 * r + 1) as u32;
        self.spend(side * side / SEARCH_DIV)?;
        let (lx, ly) = (lx(self.ctx.cell), ly(self.ctx.cell));
        let mut n = 0;
        for dy in -r..=r {
            for dx in -r..=r {
                n += i32::from(self.ctx.halo.matches(lx, ly, dx, dy, pred));
            }
        }
        Ok(n)
    }

    /// Nearest matching cell in rings `1..=r`: row-major inside a ring,
    /// the ring's start rotated by one draw so equidistant ties do not
    /// lock a flock onto one target.
    fn nearest(&mut self, pred: i32, r: i32) -> Result<Option<(i32, i32)>, Trap> {
        let r = r.clamp(0, i32::from(self.ctx.kind.sight));
        let side = (2 * r + 1) as u32;
        self.spend(side * side / SEARCH_DIV)?;
        let (lx, ly) = (lx(self.ctx.cell), ly(self.ctx.cell));
        for ring in 1..=r {
            let n = 8 * ring;
            let start = (self.draw() % n as u64) as i32;
            for k in 0..n {
                let (dx, dy) = ring_cell(ring, (start + k) % n);
                if self.ctx.halo.matches(lx, ly, dx, dy, pred) {
                    return Ok(Some((dx, dy)));
                }
            }
        }
        Ok(None)
    }

    fn act(&mut self, a: u8) -> Result<(), Trap> {
        if self.out.action != Action::Idle || self.out.trap.is_some() {
            return Err(Trap::SecondAction);
        }
        let action = Action::from_u8(a).ok_or(Trap::BadAction)?;
        match action {
            Action::Idle | Action::Die => {}
            Action::Become => {
                let kind = self.pop()?;
                self.out.kind = u16::try_from(kind).map_err(|_| Trap::BadAction)?;
            }
            Action::Spawn => {
                let dy = self.pop()?;
                let dx = self.pop()?;
                let kind = self.pop()?;
                self.out.kind = u16::try_from(kind).map_err(|_| Trap::BadAction)?;
                self.out.dx = i8::try_from(dx).map_err(|_| Trap::BadAction)?;
                self.out.dy = i8::try_from(dy).map_err(|_| Trap::BadAction)?;
            }
            Action::Move | Action::Drink | Action::Eat | Action::Hit => {
                let dy = self.pop()?;
                let dx = self.pop()?;
                // Far targets are fine: Think reduces a move to one step,
                // Resolve refuses a bite that is not adjacent.
                self.out.dx = dx.clamp(-127, 127) as i8;
                self.out.dy = dy.clamp(-127, 127) as i8;
            }
        }
        // `Idle` is an explicit action too: it ends the rule.
        self.out.action = action;
        self.acted = true;
        Ok(())
    }

    fn run(&mut self, code: &[Op], consts: &[i32], subs: &[u32]) -> Result<(), Trap> {
        use OpCode as O;
        loop {
            let op = *code.get(self.pc).ok_or(Trap::BadPc)?;
            self.pc += 1;
            self.spend(1)?;
            self.out.used += 1;
            match op.code {
                O::Push => self.push(i32::from(op.imm))?,
                O::PushK => {
                    let v = *consts.get(op.imm as u16 as usize).ok_or(Trap::BadConst)?;
                    self.push(v)?;
                }
                O::Pop => {
                    self.pop()?;
                }
                O::Load => {
                    let i = self.local(op.a)?;
                    self.push(self.locals[i])?;
                }
                O::Store => {
                    let i = self.local(op.a)?;
                    self.locals[i] = self.pop()?;
                }
                O::Need => {
                    let i = usize::from(op.a);
                    if i >= self.ctx.kind.needs.len() {
                        return Err(Trap::BadNeed);
                    }
                    self.push(self.mind.needs[i])?;
                }
                O::SetNeed => {
                    let i = usize::from(op.a);
                    let max = self.ctx.kind.needs.get(i).ok_or(Trap::BadNeed)?.max;
                    let v = self.pop()?;
                    self.mind.needs[i] = v.clamp(0, max);
                }
                O::Mem => {
                    let i = usize::from(op.a);
                    if i >= MEM_SLOTS {
                        return Err(Trap::BadMem);
                    }
                    self.push(self.mind.mem[i])?;
                }
                O::SetMem => {
                    let i = usize::from(op.a);
                    if i >= MEM_SLOTS {
                        return Err(Trap::BadMem);
                    }
                    self.mind.mem[i] = self.pop()?;
                }
                O::Sense => {
                    let s = Sense::from_u8(op.a).ok_or(Trap::BadSense)?;
                    let v = self.sense(s);
                    self.push(v)?;
                }
                O::Add
                | O::Sub
                | O::Mul
                | O::Div
                | O::Mod
                | O::Lt
                | O::Le
                | O::Eq
                | O::Ne
                | O::Ge
                | O::Gt
                | O::And
                | O::Or
                | O::Min
                | O::Max => {
                    let y = self.pop()?;
                    let x = self.pop()?;
                    let v = match op.code {
                        O::Add => x.wrapping_add(y),
                        O::Sub => x.wrapping_sub(y),
                        O::Mul => x.wrapping_mul(y),
                        O::Div => {
                            if y == 0 {
                                0
                            } else {
                                x.wrapping_div(y)
                            }
                        }
                        O::Mod => {
                            if y == 0 {
                                0
                            } else {
                                x.wrapping_rem(y)
                            }
                        }
                        O::Lt => i32::from(x < y),
                        O::Le => i32::from(x <= y),
                        O::Eq => i32::from(x == y),
                        O::Ne => i32::from(x != y),
                        O::Ge => i32::from(x >= y),
                        O::Gt => i32::from(x > y),
                        O::And => i32::from(x != 0 && y != 0),
                        O::Or => i32::from(x != 0 || y != 0),
                        O::Min => x.min(y),
                        _ => x.max(y),
                    };
                    self.push(v)?;
                }
                O::Neg => {
                    let x = self.pop()?;
                    self.push(x.wrapping_neg())?;
                }
                O::Not => {
                    let x = self.pop()?;
                    self.push(i32::from(x == 0))?;
                }
                O::Abs => {
                    let x = self.pop()?;
                    self.push(x.wrapping_abs())?;
                }
                O::Sign => {
                    let x = self.pop()?;
                    self.push(x.signum())?;
                }
                O::Clamp => {
                    let hi = self.pop()?;
                    let lo = self.pop()?;
                    let x = self.pop()?;
                    self.push(if lo <= hi { x.clamp(lo, hi) } else { lo })?;
                }
                O::Jmp => self.pc = jump(self.pc, op.imm)?,
                O::Jz => {
                    if self.pop()? == 0 {
                        self.pc = jump(self.pc, op.imm)?;
                    }
                }
                O::Jnz => {
                    if self.pop()? != 0 {
                        self.pc = jump(self.pc, op.imm)?;
                    }
                }
                O::Call => {
                    if self.depth == FRAMES {
                        return Err(Trap::CallDepth);
                    }
                    let target = *subs.get(op.imm as u16 as usize).ok_or(Trap::BadSub)? as usize;
                    let args = usize::from(op.a);
                    if args > FRAME_LOCALS || self.sp < args {
                        return Err(Trap::StackUnderflow);
                    }
                    let new_base = self.base + FRAME_LOCALS;
                    if new_base + FRAME_LOCALS > LOCALS {
                        return Err(Trap::CallDepth);
                    }
                    self.frames[self.depth] = (self.pc, self.base);
                    self.depth += 1;
                    self.sp -= args;
                    self.locals[new_base..new_base + args]
                        .copy_from_slice(&self.stack[self.sp..self.sp + args]);
                    for l in &mut self.locals[new_base + args..new_base + FRAME_LOCALS] {
                        *l = 0;
                    }
                    self.base = new_base;
                    self.pc = target;
                }
                O::Ret => {
                    if self.depth == 0 {
                        // Returning from the entry: the think is over.
                        return Ok(());
                    }
                    let value = if op.a == 1 { Some(self.pop()?) } else { None };
                    self.depth -= 1;
                    let (pc, base) = self.frames[self.depth];
                    self.pc = pc;
                    self.base = base;
                    if let Some(v) = value {
                        self.push(v)?;
                    }
                }
                O::Rand => {
                    let n = self.pop()?;
                    let v = if n <= 0 {
                        0
                    } else {
                        (self.draw() % n as u64) as i32
                    };
                    self.push(v)?;
                }
                O::Chance => {
                    let p = self.pop()?;
                    let v = i32::from((self.draw() % 100) < p.clamp(0, 100) as u64);
                    self.push(v)?;
                }
                O::Count => {
                    let r = self.pop()?;
                    let pred = self.pop()?;
                    let n = self.count(pred, r)?;
                    self.push(n)?;
                }
                O::Nearest => {
                    let r = self.pop()?;
                    let pred = self.pop()?;
                    let i = self.local(op.a)?;
                    if usize::from(op.a) + 1 >= FRAME_LOCALS {
                        return Err(Trap::BadLocal);
                    }
                    match self.nearest(pred, r)? {
                        Some((dx, dy)) => {
                            self.locals[i] = dx;
                            self.locals[i + 1] = dy;
                            self.push(1)?;
                        }
                        None => self.push(0)?,
                    }
                }
                O::Dist => {
                    let dy = self.pop()?;
                    let dx = self.pop()?;
                    self.push(dx.wrapping_abs().max(dy.wrapping_abs()))?;
                }
                O::FreeAt => {
                    let dy = self.pop()?;
                    let dx = self.pop()?;
                    let (lx, ly) = (lx(self.ctx.cell), ly(self.ctx.cell));
                    self.push(i32::from(self.ctx.halo.free(lx, ly, dx, dy)))?;
                }
                O::IsAt => {
                    let pred = self.pop()?;
                    let dy = self.pop()?;
                    let dx = self.pop()?;
                    let (lx, ly) = (lx(self.ctx.cell), ly(self.ctx.cell));
                    self.push(i32::from(self.ctx.halo.matches(lx, ly, dx, dy, pred)))?;
                }
                O::DirOf => {
                    let i = self.pop()?;
                    let (dx, dy) = usize::try_from(i - 1)
                        .ok()
                        .and_then(|i| DIRS8.get(i))
                        .copied()
                        .unwrap_or((0, 0));
                    self.push(dx)?;
                    self.push(dy)?;
                }
                O::SetLook => {
                    let v = self.pop()?;
                    self.out.look = Some(v.clamp(0, 255) as u8);
                }
                O::Act => self.act(op.a)?,
                O::Next => {
                    if usize::from(op.a) >= self.ctx.kind.states.max(1) as usize {
                        return Err(Trap::BadAction);
                    }
                    self.out.next = Some(op.a);
                }
                O::EndRule => {
                    if self.acted || self.out.next.is_some() {
                        return Ok(());
                    }
                }
                O::Halt => return Ok(()),
            }
        }
    }
}

#[inline]
fn jump(pc: usize, imm: i16) -> Result<usize, Trap> {
    usize::try_from(pc as i64 + i64::from(imm)).map_err(|_| Trap::BadPc)
}

#[inline]
fn lx(cell: usize) -> i32 {
    (cell as i32) & (CHUNK_SIZE - 1)
}

#[inline]
fn ly(cell: usize) -> i32 {
    (cell as i32) >> CHUNK_BITS
}

/// The `k`-th cell (0-based) on the perimeter of the square of radius `r`,
/// walked clockwise from the top-left corner. `8r` cells per ring.
#[inline]
fn ring_cell(r: i32, k: i32) -> (i32, i32) {
    let side = 2 * r;
    match k / side {
        0 => (-r + k, -r),             // top edge, left to right
        1 => (r, -r + (k - side)),     // right edge, top to bottom
        2 => (r - (k - 2 * side), r),  // bottom edge, right to left
        _ => (-r, r - (k - 3 * side)), // left edge, bottom to top
    }
}

/// The eight directions clockwise from north; `hurt_dir` is an index into
/// this plus one (0 = none).
pub const DIRS8: [(i32, i32); 8] = [
    (0, -1),
    (1, -1),
    (1, 0),
    (1, 1),
    (0, 1),
    (-1, 1),
    (-1, 0),
    (-1, -1),
];

/// `hurt_dir` of a unit step `(dx, dy)`: its index in [`DIRS8`] plus one,
/// 0 for `(0, 0)` or anything that is not a unit step.
#[inline]
pub fn dir_index(dx: i32, dy: i32) -> u8 {
    DIRS8
        .iter()
        .position(|&d| d == (dx, dy))
        .map_or(0, |i| i as u8 + 1)
}

/// The RNG stream base for `uid` at `tick`: every draw of the think is
/// `splitmix64(base + n)`.
pub const STREAM_THINK: u64 = 0x0010;

#[inline]
pub fn rng_base(seed: u64, tick: u64, uid: u64) -> u64 {
    splitmix64(seed ^ STREAM_THINK ^ splitmix64(tick) ^ uid)
}

/// Run one think: the kind's program from its entry, against `mind`. The
/// mind's needs/mem/state are updated in place (a trap leaves what was
/// written before it); the action comes back in the [`Outcome`].
pub fn think(program: &super::Kinds, ctx: Ctx<'_>, mind: &mut ActorMind) -> Outcome {
    let mut m = Machine {
        stack: [0; STACK],
        sp: 0,
        locals: [0; LOCALS],
        base: 0,
        frames: [(0, 0); FRAMES],
        depth: 0,
        pc: ctx.kind.entry as usize,
        fuel: ctx.kind.fuel,
        draws: 0,
        out: Outcome::default(),
        acted: false,
        ctx,
        mind,
    };
    if let Err(trap) = m.run(&program.code, &program.consts, &program.subs) {
        m.out.trap = Some(trap);
        m.out.action = Action::Idle;
        m.out.next = None;
        m.out.look = None;
    }
    m.out
}

/// Decay a mind's consumable needs by the ticks since its last think and
/// stamp it. Returns `true` if a vital need is empty (the actor dies).
pub fn decay(kind: &KindDef, mind: &mut ActorMind, tick: u64) -> bool {
    let now = tick as u32;
    let elapsed = i64::from(now.wrapping_sub(mind.last_think));
    mind.last_think = now;
    let mut dead = false;
    for (i, need) in kind.needs.iter().enumerate().take(NEED_SLOTS) {
        if need.decays {
            let v = (i64::from(mind.needs[i]) - elapsed).max(0);
            mind.needs[i] = v as i32;
        }
        dead |= need.vital && mind.needs[i] <= 0;
    }
    dead
}

/// Where `(dx, dy)` from local cell `cell` lands: the chunk offset
/// (`-1..=1` each way, or beyond) and the local index there.
#[inline]
pub fn offset_cell(cell: usize, dx: i32, dy: i32) -> ((i32, i32), usize) {
    let (x, y) = (lx(cell) + dx, ly(cell) + dy);
    let local = ((y & (CHUNK_SIZE - 1)) * CHUNK_SIZE + (x & (CHUNK_SIZE - 1))) as usize;
    ((x >> CHUNK_BITS, y >> CHUNK_BITS), local)
}

/// Is a live occupant of `kind` here? (Convenience for tests and tools.)
pub fn occupant_kind(id: ActorId) -> Option<u16> {
    id.unpack().map(|(k, _)| k)
}

// Ground/Feature discriminants are what `pred::ground`/`pred::feature` take.
const _: () = assert!(Ground::Soil as u8 == 0 && Ground::Water as u8 == 1);
const _: () = assert!(Feature::None as u8 == 0 && Feature::Rock as u8 == 1);

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rules::asm::Asm;
    use crate::rules::{KindDef, Kinds, NeedDef};
    use crate::stage::CHUNK_CELLS;
    use bytemuck::Zeroable;

    fn kind(needs: Vec<NeedDef>, entry: u32) -> KindDef {
        KindDef {
            id: 0,
            name: "t".into(),
            glyph: b't',
            tags: 0,
            cadence_shift: 0,
            sight: 8,
            fuel: 512,
            food: 0,
            bite: 1,
            needs,
            mems: vec![],
            states: 1,
            entry,
            place: 0,
        }
    }

    fn need(max: i32, decays: bool, vital: bool) -> NeedDef {
        NeedDef {
            name: "n".into(),
            max,
            decays,
            vital,
        }
    }

    /// One chunk of soil with water at local (10, 10) and a rock at (12, 10);
    /// the actor stands at (11, 10).
    fn stage() -> (ChunkCells, ChunkActors) {
        let mut cells = ChunkCells::default();
        cells.ground[10 * 64 + 10] = Ground::Water;
        cells.feature[10 * 64 + 12] = Feature::Rock;
        cells.occupant[10 * 64 + 11] = ActorId::pack(0, 0);
        cells.occupant[12 * 64 + 11] = ActorId::pack(3, 0);
        (cells, ChunkActors::default())
    }

    fn run(code: Vec<Op>, consts: Vec<i32>, needs: Vec<NeedDef>, mind: &mut ActorMind) -> Outcome {
        let (cells, actors) = stage();
        let halo = Halo {
            chunks: [
                None,
                None,
                None,
                None,
                Some((&cells, &actors)),
                None,
                None,
                None,
                None,
            ],
            tags: &[],
        };
        let k = kind(needs, 0);
        let kinds = Kinds::from_parts(vec![k], code, consts, vec![]);
        let ctx = Ctx {
            halo: &halo,
            kind: &kinds.defs[0],
            cell: 10 * 64 + 11,
            pos: Pos::new(11, 10),
            tick: 1000,
            rng: rng_base(1, 1000, 7),
            look: 0,
            signal: 0,
        };
        think(&kinds, ctx, mind)
    }

    fn mind() -> ActorMind {
        ActorMind::zeroed()
    }

    #[test]
    fn arithmetic_is_wrapping_and_division_by_zero_is_zero() {
        let mut a = Asm::new();
        a.push(7).push(2).op(OpCode::Div).set_mem(0);
        a.push(7).push(0).op(OpCode::Div).set_mem(1);
        a.push(7).push(0).op(OpCode::Mod).set_mem(2);
        a.push_k(0).push(1).op(OpCode::Add).set_mem(3); // i32::MAX + 1 wraps
        a.push(-5).op(OpCode::Abs).set_mem(4);
        a.push(9).push(0).push(4).op(OpCode::Clamp).set_mem(5);
        a.push(3)
            .push(4)
            .op(OpCode::Lt)
            .push(4)
            .push(3)
            .op(OpCode::Ge)
            .op(OpCode::And)
            .set_mem(6);
        a.push(0).op(OpCode::Not).set_mem(7);
        a.push(-3).op(OpCode::Sign).set_mem(8);
        a.halt();
        let mut m = mind();
        let out = run(a.finish(), vec![i32::MAX], vec![], &mut m);
        assert_eq!(out.trap, None);
        assert_eq!(&m.mem[..9], &[3, 0, 0, i32::MIN, 5, 4, 1, 1, -1]);
        assert_eq!(out.action, Action::Idle);
    }

    #[test]
    fn rules_fall_through_until_one_acts() {
        // when 0 => die ; when 1 => mem0 = 1 (no action, falls through) ; when 1 => become 2
        let mut a = Asm::new();
        let l1 = a.label();
        a.push(0).jz(l1).act(Action::Die).end_rule();
        a.bind(l1);
        let l2 = a.label();
        a.push(1).jz(l2).push(1).set_mem(0).end_rule();
        a.bind(l2);
        let l3 = a.label();
        a.push(1).jz(l3).push(2).act(Action::Become).end_rule();
        a.bind(l3);
        a.push(9).set_mem(1).halt(); // never reached
        let mut m = mind();
        let out = run(a.finish(), vec![], vec![], &mut m);
        assert_eq!(out.trap, None);
        assert_eq!(out.action, Action::Become);
        assert_eq!(out.kind, 2);
        assert_eq!((m.mem[0], m.mem[1]), (1, 0));
    }

    #[test]
    fn explicit_idle_and_next_end_the_think_too() {
        let mut a = Asm::new();
        a.act(Action::Idle).end_rule().push(1).set_mem(0).halt();
        let mut m = mind();
        let out = run(a.finish(), vec![], vec![], &mut m);
        assert_eq!((out.action, out.trap, m.mem[0]), (Action::Idle, None, 0));
        let mut a = Asm::new();
        a.next(0).end_rule().push(1).set_mem(0).halt();
        let out = run(a.finish(), vec![], vec![], &mut m);
        assert_eq!((out.next, m.mem[0]), (Some(0), 0));
    }

    #[test]
    fn a_second_action_traps_to_idle() {
        let mut a = Asm::new();
        a.act(Action::Die).push(1).act(Action::Become).halt();
        let out = run(a.finish(), vec![], vec![], &mut mind());
        assert_eq!(out.trap, Some(Trap::SecondAction));
        assert_eq!(out.action, Action::Idle);
    }

    #[test]
    fn fuel_bounds_a_loop_and_stack_faults_trap() {
        let mut a = Asm::new();
        let top = a.label();
        a.bind(top);
        a.mem(0).push(1).op(OpCode::Add).set_mem(0).jmp(top);
        let mut m = mind();
        let out = run(a.finish(), vec![], vec![], &mut m);
        assert_eq!(out.trap, Some(Trap::Fuel));
        assert_eq!(out.used, 512);
        assert!(
            m.mem[0] > 50,
            "the loop ran until the fuel was gone: {}",
            m.mem[0]
        );
        let mut a = Asm::new();
        a.op(OpCode::Pop).halt();
        assert_eq!(
            run(a.finish(), vec![], vec![], &mut mind()).trap,
            Some(Trap::StackUnderflow)
        );
        let mut a = Asm::new();
        let top = a.label();
        a.bind(top);
        a.push(1).jmp(top);
        assert_eq!(
            run(a.finish(), vec![], vec![], &mut mind()).trap,
            Some(Trap::StackOverflow)
        );
        let mut a = Asm::new();
        a.jmp_raw(-100);
        assert_eq!(
            run(a.finish(), vec![], vec![], &mut mind()).trap,
            Some(Trap::BadPc)
        );
        assert_eq!(
            run(vec![], vec![], vec![], &mut mind()).trap,
            Some(Trap::BadPc)
        );
        let mut a = Asm::new();
        a.push_k(3).halt();
        assert_eq!(
            run(a.finish(), vec![], vec![], &mut mind()).trap,
            Some(Trap::BadConst)
        );
        let mut a = Asm::new();
        a.need(0).halt();
        assert_eq!(
            run(a.finish(), vec![], vec![], &mut mind()).trap,
            Some(Trap::BadNeed)
        );
    }

    #[test]
    fn needs_are_clamped_and_senses_read_the_world() {
        let mut a = Asm::new();
        a.push(500).set_need(0); // clamped to max 100
        a.push(-5).set_need(1); // clamped to 0
        a.sense(Sense::Light).set_mem(0);
        a.sense(Sense::X).set_mem(1);
        a.sense(Sense::Y).set_mem(2);
        a.sense(Sense::Ground).set_mem(3);
        a.sense(Sense::Hour).set_mem(4);
        a.push(pred::ground(1)).push(2).op(OpCode::Count).set_mem(5); // water within 2
        a.push(pred::feature(1))
            .push(1)
            .op(OpCode::Count)
            .set_mem(6); // rock within 1
        a.push(pred::FREE).push(1).op(OpCode::Count).set_mem(7); // free within 1
        a.push(3).push(2).op(OpCode::Count).set_mem(8); // kind 3 within 2
        a.push(pred::FREE).push(100).op(OpCode::Count).set_mem(9); // clamped to sight
        a.halt();
        let mut m = mind();
        m.needs = [50, 50, 0, 0];
        let out = run(
            a.finish(),
            vec![],
            vec![need(100, true, false), need(10, false, false)],
            &mut m,
        );
        assert_eq!(out.trap, None);
        assert_eq!(m.needs[0], 100);
        assert_eq!(m.needs[1], 0);
        assert_eq!(m.mem[0], i32::from(daylight(1000)));
        assert_eq!((m.mem[1], m.mem[2]), (11, 10));
        assert_eq!(m.mem[3], pred::ground(0));
        assert_eq!(m.mem[4], i32::from(Clock::at(1000).hour));
        assert_eq!(m.mem[5], 1);
        assert_eq!(m.mem[6], 1);
        // 3x3 around (11,10): 9 cells minus self, water, rock = 6 free.
        assert_eq!(m.mem[7], 6);
        assert_eq!(m.mem[8], 1);
        // Radius clamped to sight 8: 17x17 square minus the water, rock,
        // self and the kind-3 actor; the chunk edge at x<3 is off the halo
        // (unloaded = wall) so those columns do not count.
        let expect = (3..=19)
            .flat_map(|x| (2..=18).map(move |y| (x, y)))
            .filter(|&(x, y)| {
                !((x == 10 && y == 10)
                    || (x == 12 && y == 10)
                    || (x == 11 && y == 10)
                    || (x == 11 && y == 12))
            })
            .count() as i32;
        assert_eq!(m.mem[9], expect);
    }

    #[test]
    fn nearest_binds_the_closest_and_rotates_ties() {
        let mut a = Asm::new();
        let l = a.label();
        a.push(pred::ground(1)).push(3).nearest(0).jz(l);
        a.load(0).set_mem(0).load(1).set_mem(1).push(1).set_mem(2);
        a.bind(l);
        a.push(pred::feature(1)).push(5).nearest(2).set_mem(3);
        a.load(2).set_mem(4);
        a.push(77).push(2).nearest(4).set_mem(5); // nothing of kind 77
        a.halt();
        let mut m = mind();
        let out = run(a.finish(), vec![], vec![], &mut m);
        assert_eq!(out.trap, None);
        assert_eq!(&m.mem[..6], &[-1, 0, 1, 1, 1, 0]);
        // Ties: many free cells at distance 1; the one bound depends on the
        // draw, and the draw on (seed, tick, uid), so it is stable.
        let mut a = Asm::new();
        a.push(pred::FREE)
            .push(1)
            .nearest(0)
            .set_mem(9)
            .load(0)
            .set_mem(0)
            .load(1)
            .set_mem(1)
            .halt();
        let code = a.finish();
        let mut m1 = mind();
        run(code.clone(), vec![], vec![], &mut m1);
        let mut m2 = mind();
        run(code, vec![], vec![], &mut m2);
        assert_eq!((m1.mem[0], m1.mem[1]), (m2.mem[0], m2.mem[1]));
        assert_eq!(m1.mem[9], 1);
        assert!(m1.mem[0].abs() <= 1 && m1.mem[1].abs() <= 1 && (m1.mem[0], m1.mem[1]) != (0, 0));
    }

    #[test]
    fn random_draws_are_a_pure_function_of_the_stream() {
        let mut a = Asm::new();
        a.push(1000).op(OpCode::Rand).set_mem(0);
        a.push(1000).op(OpCode::Rand).set_mem(1);
        a.push(0).op(OpCode::Rand).set_mem(2);
        a.push(100).op(OpCode::Chance).set_mem(3);
        a.push(0).op(OpCode::Chance).set_mem(4);
        a.halt();
        let code = a.finish();
        let mut m1 = mind();
        run(code.clone(), vec![], vec![], &mut m1);
        let mut m2 = mind();
        run(code, vec![], vec![], &mut m2);
        assert_eq!(m1.mem, m2.mem);
        assert_ne!(m1.mem[0], m1.mem[1]);
        assert!((0..1000).contains(&m1.mem[0]));
        assert_eq!((m1.mem[2], m1.mem[3], m1.mem[4]), (0, 1, 0));
    }

    #[test]
    fn calls_pass_arguments_and_return_values() {
        // entry: mem0 = twice(21); halt.  twice(x): return x * 2
        let mut a = Asm::new();
        a.push(21).call(0, 1).set_mem(0).halt();
        let twice = a.here();
        a.load(0).push(2).op(OpCode::Mul).ret(true);
        let code = a.finish();
        let k = kind(vec![], 0);
        let kinds = Kinds::from_parts(vec![k], code, vec![], vec![twice]);
        let (cells, actors) = stage();
        let halo = Halo {
            chunks: [
                None,
                None,
                None,
                None,
                Some((&cells, &actors)),
                None,
                None,
                None,
                None,
            ],
            tags: &[],
        };
        let ctx = Ctx {
            halo: &halo,
            kind: &kinds.defs[0],
            cell: 10 * 64 + 11,
            pos: Pos::new(11, 10),
            tick: 5,
            rng: 0,
            look: 0,
            signal: 0,
        };
        let mut m = mind();
        let out = think(&kinds, ctx, &mut m);
        assert_eq!(out.trap, None);
        assert_eq!(m.mem[0], 42);
        // Unbounded recursion trips the depth limit, not the stack.
        let mut a = Asm::new();
        a.call(0, 0).halt();
        let f = a.here();
        a.call(0, 0).ret(false);
        let kinds = Kinds::from_parts(vec![kind(vec![], 0)], a.finish(), vec![], vec![f]);
        let ctx = Ctx {
            kind: &kinds.defs[0],
            ..ctx
        };
        assert_eq!(think(&kinds, ctx, &mut m).trap, Some(Trap::CallDepth));
    }

    #[test]
    fn decay_is_exact_and_kills_at_zero() {
        let k = kind(
            vec![
                need(100, true, true),
                need(10, false, true),
                need(5, true, false),
            ],
            0,
        );
        let mut m = mind();
        m.needs = [30, 10, 5, 0];
        m.last_think = 100;
        assert!(!decay(&k, &mut m, 120));
        assert_eq!(m.needs, [10, 10, 0, 0]);
        assert_eq!(m.last_think, 120);
        assert!(!decay(&k, &mut m, 125));
        assert_eq!(m.needs[0], 5);
        assert!(decay(&k, &mut m, 130));
        assert_eq!(m.needs[0], 0);
        // Points needs do not decay but are vital at zero.
        m.needs = [50, 0, 0, 0];
        assert!(decay(&k, &mut m, 131));
        // Wrapping ticks.
        m.needs = [50, 1, 0, 0];
        m.last_think = u32::MAX - 1;
        assert!(!decay(&k, &mut m, u64::from(u32::MAX) + 3)); // 4 ticks later
        assert_eq!(m.needs[0], 46);
    }

    #[test]
    fn halo_addressing_crosses_into_neighbours() {
        let (cells, actors) = stage();
        let mut halo = Halo {
            chunks: [None; 9],
            tags: &[],
        };
        halo.chunks[4] = Some((&cells, &actors));
        halo.chunks[5] = Some((&cells, &actors)); // east neighbour
        // From local (63, 5), +1 in x lands in the east chunk at (0, 5).
        let (_, _, i) = halo.at(63, 5, 1, 0).unwrap();
        assert_eq!(i, 5 * 64);
        assert!(halo.at(63, 5, 1, -6).is_none()); // north-east: not loaded
        assert!(halo.at(0, 0, -1, 0).is_none()); // west: not loaded
        assert_eq!(offset_cell(5 * 64 + 63, 1, 0), ((1, 0), 5 * 64));
        assert_eq!(offset_cell(0, -1, -1), ((-1, -1), CHUNK_CELLS - 1));
        assert_eq!(offset_cell(70, 2, 3), ((0, 0), 70 + 2 + 3 * 64));
        // Every ring cell is on the ring and distinct.
        for r in 1..=4 {
            let cells: Vec<_> = (0..8 * r).map(|k| ring_cell(r, k)).collect();
            assert!(cells.iter().all(|&(x, y)| x.abs().max(y.abs()) == r));
            let mut u = cells.clone();
            u.sort_unstable();
            u.dedup();
            assert_eq!(u.len(), cells.len());
        }
    }
}
