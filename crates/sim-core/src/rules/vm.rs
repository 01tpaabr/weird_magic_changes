//! The rules virtual machine: a fuel-bounded integer stack machine that runs
//! one actor's think (`docs/ACTORS.md` §5).
//!
//! A program is a flat `Vec<Op>` shared read-only by every thread. A think
//! runs a kind's entry point from the top with an empty stack; the only state
//! that persists is the actor's row (needs, mem, state). The machine can
//! read the tick-start world through a [`Halo`] (own chunk and the eight
//! around it), read and write its own row, draw from a counter-based RNG,
//! and emit at most one [`Action`] plus a `next` state. Every op costs one
//! unit of fuel, a search costs `(2r+1)^2 / 8` more; fuel out, a bad jump, a
//! stack fault or a second action end the think with `idle` and a
//! [`Trap`]. The VM never panics on a program.
//!
//! Rule structure is compiled to jumps: `cond; Jz next; body; EndRule`.
//! `EndRule` halts if the body emitted an action or a `next`, otherwise
//! execution falls through to the next rule. Falling off the end is `idle`.

use crate::actors::{ActorMind, ActorPub, ChunkActors, MEM_SLOTS, NEED_SLOTS};
use crate::rng::splitmix64;
use crate::stage::{
    ActorId, CHUNK_BITS, CHUNK_SIZE, ChunkCells, ChunkCoord, Feature, Ground, Pos, SCENT_CHANNELS,
};
use crate::time::{Clock, daylight};

use super::{KindDef, Kinds};

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
    Lt,
    Le,
    Eq,
    Ne,
    Ge,
    Gt,
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
    /// `v ->`; set own `signal`, clamped to `i16` (an effect, like `SetLook`)
    SetSignal,
    /// `dx dy -> look` of whoever stands there, else of the cover there; 0
    /// for nobody
    LookAt,
    /// `dx dy -> signal` of whoever stands there, else of the cover there;
    /// 0 for nobody
    SignalAt,
    /// `hi lo -> hi * 256 + (lo & 255)`: two signed bytes in one value
    Pack,
    /// `v -> v >> 8` (arithmetic): the first byte of a `pack`
    Hi,
    /// `v -> v` sign-extended from its low byte: the second byte of a `pack`
    Lo,
    /// `a b ->`: the next `spawn`'s child starts with `mem[0] = a`,
    /// `mem[1] = b` (`spawn kind at t with (a, b)`)
    SpawnWith,
    /// `v ->`: add `v` (clamped to `0..=255`) to scent channel `a` of the
    /// actor's cell, saturating (an effect, applied in Apply)
    Mark,
    /// `dx dy -> scent` of channel `a` at that cell (0 where not loaded)
    ScentAt,
    /// `ch r -> found`; on success `locals[a], locals[a+1] = dx, dy` of the
    /// cell with the most of scent `ch` in rings `1..=r` (the first such in
    /// scan order; each ring's start rotated by one draw); not found if
    /// every cell there has none
    Sniff,
    /// One step of `for each`. Locals `a..a+5` hold `dx, dy, cursor, pred,
    /// r`. `-> found`: the first matching cell at or after the cursor, in
    /// ring order (rings `1..=r`, each clockwise from its top-left corner),
    /// is bound into `dx, dy` and the cursor moves past it. The first step
    /// (cursor 0) pays the search's fuel.
    ForEach,
    /// `-> found`; on success `locals[a], locals[a+1] = dx, dy` of a free
    /// neighbour (`random free`): ring 1 as `Nearest` scans it, start
    /// rotated by one draw, whatever the kind's `sight`
    RandomFree,
}

impl OpCode {
    /// `(pops, pushes)` of this op with operand `a`, as the comments above
    /// say. A `Call` pushes nothing here: what the callee returns is its
    /// `Ret`'s, counted by the caller (`Asm::returned`).
    pub fn stack_effect(self, a: u8) -> (u8, u8) {
        use OpCode as O;
        match self {
            O::Push | O::PushK | O::Load | O::Need | O::Mem | O::Sense | O::ForEach => (0, 1),
            O::RandomFree => (0, 1),
            O::Pop | O::Store | O::SetNeed | O::SetMem | O::Jz => (1, 0),
            O::SetLook | O::SetSignal | O::Mark => (1, 0),
            O::Add | O::Sub | O::Mul | O::Div | O::Mod => (2, 1),
            O::Lt | O::Le | O::Eq | O::Ne | O::Ge | O::Gt => (2, 1),
            O::Min | O::Max | O::Pack => (2, 1),
            O::Neg | O::Abs | O::Sign | O::Hi | O::Lo | O::Rand | O::Chance => (1, 1),
            O::Clamp | O::IsAt => (3, 1),
            O::Count | O::Nearest | O::Sniff | O::Dist | O::FreeAt => (2, 1),
            O::LookAt | O::SignalAt | O::ScentAt => (2, 1),
            O::DirOf => (1, 2),
            O::SpawnWith => (2, 0),
            O::Jmp | O::Next | O::EndRule | O::Halt => (0, 0),
            O::Call => (a, 0),
            O::Ret => (u8::from(a == 1), 0),
            O::Act => match Action::from_u8(a) {
                Some(Action::Become) => (1, 0),
                Some(Action::Spawn) => (3, 0),
                Some(Action::Take | Action::Give) => (4, 0),
                Some(Action::Move | Action::Drink | Action::Eat | Action::Hit | Action::Graze) => {
                    (2, 0)
                }
                Some(Action::Idle | Action::Die) | None => (0, 0),
            },
        }
    }
}

/// Locals a `for each` loop keeps: `dx, dy, cursor, pred, r`.
pub const FOR_EACH_LOCALS: u8 = 5;

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
    /// 1 if something was taken from this actor since its last think
    Taken,
    /// 1 if the last think trapped (fuel out, a second action, a fault)
    Trapped,
}

impl Sense {
    /// Every sense, in numbering order.
    pub const ALL: [Sense; 15] = [
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
        Self::Taken,
        Self::Trapped,
    ];

    pub fn from_u8(a: u8) -> Option<Self> {
        Self::ALL.get(usize::from(a)).copied()
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
    /// `dx dy`: refill `water` from the adjacent water cell there
    Drink,
    /// `dx dy`: bite the adjacent actor there (`bite` off its `health`);
    /// if it dies this tick, the lowest-key eater gains its kind's `food`
    Eat,
    /// `dx dy`: bite the adjacent actor there, no food
    Hit,
    /// `dx dy`: bite the ground cover there (adjacent or underfoot) and gain
    /// the share of its `food` taken, like `eat`
    Graze,
    /// `dx dy need amount`: move up to `amount` of the adjacent actor's need
    /// with the same name as own need `need` into it
    Take,
    /// `dx dy need amount`: move up to `amount` of own need `need` into the
    /// adjacent actor's need of the same name
    Give,
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
            8 => Some(Self::Graze),
            9 => Some(Self::Take),
            10 => Some(Self::Give),
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
    /// Operand offset of every targeted action (`Spawn`, `Move`, `Drink`,
    /// `Eat`, `Hit`, `Graze`, `Take`, `Give`).
    pub dx: i8,
    pub dy: i8,
    /// Own need slot of `Take` / `Give`.
    pub need: u8,
    /// Amount of `Take` / `Give`.
    pub amount: i32,
    /// First two `mem` values of a `Spawn`'s child (`with (a, b)`).
    pub with: [i32; 2],
    /// `mark ch v` effects: what to add to each scent channel (several
    /// marks add up, saturating).
    pub mark: [u8; SCENT_CHANNELS],
    pub next: Option<u8>,
    /// `look = v` effect, if the think set one.
    pub look: Option<u8>,
    /// `signal = v` effect, if the think set one.
    pub signal: Option<i16>,
    pub trap: Option<Trap>,
    /// Ops executed, for `wmc why` and the fuel counters.
    pub used: u32,
}

/// One executed op of a traced think (`wmc why`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Step {
    pub pc: u32,
    pub op: Op,
    /// Top of the stack after the op, if the stack is not empty.
    pub top: Option<i32>,
    /// Fuel left after it.
    pub fuel: u32,
}

// ---- predicates ------------------------------------------------------------------------

/// Predicate values, one `i32` at run time: a kind (`0..TAG_BASE`), a tag
/// (`TAG_BASE + tag index`, matched against the occupant kind's tag bits),
/// a kind showing a look (`LOOK_BASE + look << 16 + kind`, `flower:1`), or
/// one of these.
pub mod pred {
    /// Walkable, nobody standing there.
    pub const FREE: i32 = -1;
    /// Walkable, no ground cover there.
    pub const BARE: i32 = -2;
    pub const GROUND_BASE: i32 = -0x100;
    pub const FEATURE_BASE: i32 = -0x200;
    pub const TAG_BASE: i32 = 0x1_0000;
    pub const LOOK_BASE: i32 = 0x0100_0000;
    /// Added to a kind or `kind:look` value: that kind exactly (`only
    /// chicken`), not its family.
    pub const ONLY: i32 = 0x0200_0000;

    /// `kind:look`: that kind, showing that look byte.
    pub const fn kind_look(kind: u16, look: u8) -> i32 {
        LOOK_BASE + ((look as i32) << 16) + kind as i32
    }

    pub const fn ground(g: u8) -> i32 {
        GROUND_BASE - g as i32
    }
    pub const fn feature(f: u8) -> i32 {
        FEATURE_BASE - f as i32
    }
}

/// A predicate decoded against the kind table: what a cell must hold.
/// A search decodes its predicate once and tests every cell with it, so a
/// kind's family costs two compares per cell, like an exact kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Want {
    /// Someone of a kind in `lo..hi` (a family, or one kind) stands on or
    /// covers the cell.
    Kinds(u16, u16),
    /// The same, showing this `look`.
    KindsLook(u16, u16, u8),
    /// Someone whose kind carries tag bit `b`.
    Tag(u32),
    Free,
    Bare,
    Ground(i32),
    Feature(i32),
    /// A value no cell matches.
    Nothing,
}

impl Want {
    /// Decode `pred` (see [`pred`]) with `family_end` (`Kinds::family_end`;
    /// empty: every kind is its own family).
    pub fn of(pred: i32, family_end: &[u16]) -> Want {
        if pred < 0 {
            return match pred {
                pred::FREE => Want::Free,
                pred::BARE => Want::Bare,
                p if p > pred::FEATURE_BASE => Want::Ground(pred::GROUND_BASE - p),
                p => Want::Feature(pred::FEATURE_BASE - p),
            };
        }
        let (p, exact) = if pred >= pred::ONLY {
            (pred - pred::ONLY, true)
        } else {
            (pred, false)
        };
        // `k`'s family: `k` and every kind that extends it, one id range.
        let range = |k: i32| -> (u16, u16) {
            let lo = k as u16;
            let hi = if exact {
                u32::from(lo) + 1
            } else {
                family_end
                    .get(usize::from(lo))
                    .map_or(u32::from(lo) + 1, |&e| u32::from(e))
            };
            (lo, hi.min(u32::from(u16::MAX)) as u16)
        };
        if p >= pred::LOOK_BASE {
            let v = p - pred::LOOK_BASE;
            let (lo, hi) = range(v & 0xFFFF);
            return u8::try_from(v >> 16)
                .map_or(Want::Nothing, |look| Want::KindsLook(lo, hi, look));
        }
        if p >= pred::TAG_BASE {
            let bit = (p - pred::TAG_BASE) as u32;
            return if bit < 64 {
                Want::Tag(bit)
            } else {
                Want::Nothing
            };
        }
        let (lo, hi) = range(p);
        Want::Kinds(lo, hi)
    }

    /// Does local cell `i` hold what this wants? `actors` are the chunk's
    /// public rows (for looks), `tags` the tag bitset per kind.
    #[inline]
    pub fn test(self, cells: &ChunkCells, actors: &ChunkActors, i: usize, tags: &[u64]) -> bool {
        // A kind or a tag matches whoever stands on the cell or covers it.
        let (occupant, cover) = (cells.occupant[i], cells.cover[i]);
        match self {
            Want::Kinds(lo, hi) => {
                let hit = |id: ActorId| id.unpack().is_some_and(|(k, _)| k >= lo && k < hi);
                hit(occupant) || hit(cover)
            }
            Want::KindsLook(lo, hi, look) => {
                let hit = |id: ActorId| {
                    id.unpack().is_some_and(|(k, slot)| {
                        k >= lo
                            && k < hi
                            && actors
                                .rows
                                .get(usize::from(slot))
                                .is_some_and(|r| r.look == look)
                    })
                };
                hit(occupant) || hit(cover)
            }
            Want::Tag(bit) => {
                let hit = |id: ActorId| {
                    id.unpack().is_some_and(|(k, _)| {
                        tags.get(usize::from(k)).is_some_and(|t| t >> bit & 1 == 1)
                    })
                };
                hit(occupant) || hit(cover)
            }
            Want::Free => cells.walkable(i) && occupant.is_none(),
            Want::Bare => cells.walkable(i) && cover.is_none(),
            Want::Ground(g) => cells.ground[i] as i32 == g,
            Want::Feature(f) => cells.feature[i] as i32 == f,
            Want::Nothing => false,
        }
    }
}

// ---- the halo --------------------------------------------------------------------------

/// A chunk and its eight neighbours, as loaded at tick start. Index
/// `(oy + 1) * 3 + (ox + 1)`; `None` = not loaded (a wall: rock, nobody).
/// `tags` is the kind table's tag bitset per kind, for tag predicates.
#[derive(Debug, Clone, Copy)]
pub struct Halo<'a> {
    pub chunks: [Option<(&'a ChunkCells, &'a ChunkActors)>; 9],
    pub tags: &'a [u64],
    /// `Kinds::family_end`; empty: every kind is its own family.
    pub family_end: &'a [u16],
}

impl<'a> Halo<'a> {
    /// The halo around chunk `c`, each chunk from `get` (`None`: not
    /// loaded). The Think phase and `sim::explain` both build it here.
    #[inline]
    pub fn around(
        c: ChunkCoord,
        kinds: &'a Kinds,
        get: impl Fn(ChunkCoord) -> Option<(&'a ChunkCells, &'a ChunkActors)>,
    ) -> Halo<'a> {
        let mut chunks = [None; 9];
        for (i, slot) in chunks.iter_mut().enumerate() {
            let (ox, oy) = ((i % 3) as i32 - 1, (i / 3) as i32 - 1);
            *slot = get(ChunkCoord::new(c.x + ox, c.y + oy));
        }
        Halo {
            chunks,
            tags: &kinds.tag_bits,
            family_end: &kinds.family_end,
        }
    }

    /// The chunk and local index at `(lx + dx, ly + dy)` from the centre
    /// chunk's local cell `(lx, ly)`. `None` if it falls outside the halo
    /// (however far: a rule computes `dx`/`dy`) or the chunk there is not
    /// loaded.
    #[inline]
    pub fn at(
        &self,
        lx: i32,
        ly: i32,
        dx: i32,
        dy: i32,
    ) -> Option<(&'a ChunkCells, &'a ChunkActors, usize)> {
        let (x, y) = (lx.checked_add(dx)?, ly.checked_add(dy)?);
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
        self.test(lx, ly, dx, dy, self.want(pred))
    }

    /// Decode a predicate for [`Halo::test`].
    #[inline]
    pub fn want(&self, pred: i32) -> Want {
        Want::of(pred, self.family_end)
    }

    /// Does the cell at `(lx + dx, ly + dy)` hold what `want` wants?
    #[inline]
    pub fn test(&self, lx: i32, ly: i32, dx: i32, dy: i32, want: Want) -> bool {
        match self.at(lx, ly, dx, dy) {
            Some((cells, actors, i)) => want.test(cells, actors, i, self.tags),
            // Unloaded: rock, nobody.
            None => want == Want::Feature(Feature::Rock as i32),
        }
    }

    /// Walkable and empty at tick start.
    #[inline]
    pub fn free(&self, lx: i32, ly: i32, dx: i32, dy: i32) -> bool {
        self.test(lx, ly, dx, dy, Want::Free)
    }

    /// Scent channel `ch` at the cell; 0 where not loaded.
    #[inline]
    pub fn scent(&self, lx: i32, ly: i32, dx: i32, dy: i32, ch: usize) -> u8 {
        self.at(lx, ly, dx, dy)
            .and_then(|(cells, _, i)| cells.scent.get(ch).map(|s| s[i]))
            .unwrap_or(0)
    }

    /// The public row of whoever stands at the cell, else of its cover.
    #[inline]
    pub fn row_at(&self, lx: i32, ly: i32, dx: i32, dy: i32) -> Option<&'a ActorPub> {
        let (cells, actors, i) = self.at(lx, ly, dx, dy)?;
        let (_, slot) = cells.occupant[i].unpack().or(cells.cover[i].unpack())?;
        actors.rows.get(usize::from(slot))
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

/// Values on the stack of one think, shared by a rule and the subs it
/// calls (RULES.md §18).
pub const STACK: usize = 64;
/// Sub frames on top of the rule's own: calls nest this deep (RULES.md §12).
pub const FRAMES: usize = 8;

/// Locals per call frame (a sub's arguments and `let`s).
pub const FRAME_LOCALS: usize = 16;

/// The rule's frame and `FRAMES` sub frames: a call runs out of frames,
/// never of locals.
const LOCALS: usize = (FRAMES + 1) * FRAME_LOCALS;

/// Per-op fuel; a search costs `cells / SEARCH_DIV` extra.
pub const SEARCH_DIV: u32 = 8;

struct Machine<'m> {
    stack: [i32; STACK],
    sp: usize,
    locals: [i32; LOCALS],
    base: usize,
    frames: [(usize, usize); FRAMES],
    depth: usize,
    pc: usize,
    /// The op being run (kept only when tracing): a trap's trace ends there.
    at: usize,
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
            // Past i32::MAX days (a save may hold a tick up to 2^63) it
            // stays there rather than wrap.
            Sense::Day => i32::try_from(Clock::at(c.tick).day).unwrap_or(i32::MAX),
            Sense::Kind => i32::from(c.kind.id),
            Sense::Look => i32::from(c.look),
            Sense::Signal => i32::from(c.signal),
            Sense::State => i32::from(self.mind.state),
            Sense::Hurt => i32::from(self.mind.hurt),
            Sense::HurtDir => i32::from(self.mind.hurt_dir),
            Sense::Result => i32::from(self.mind.events & result::MASK),
            Sense::Taken => i32::from(self.mind.events & event::TAKEN != 0),
            Sense::Trapped => i32::from(self.mind.events & event::FUEL != 0),
        }
    }

    /// Cells within Chebyshev `r` of self matching `pred`, self included.
    fn count(&mut self, pred: i32, r: i32) -> Result<i32, Trap> {
        let r = r.clamp(0, i32::from(self.ctx.kind.sight));
        let side = (2 * r + 1) as u32;
        self.spend(side * side / SEARCH_DIV)?;
        let (lx, ly) = (lx(self.ctx.cell), ly(self.ctx.cell));
        let want = self.ctx.halo.want(pred);
        let mut n = 0;
        for dy in -r..=r {
            for dx in -r..=r {
                n += i32::from(self.ctx.halo.test(lx, ly, dx, dy, want));
            }
        }
        Ok(n)
    }

    /// Nearest matching cell in rings `1..=r`: each ring clockwise from its
    /// top-left corner ([`ring_cell`]), the ring's start rotated by one draw so
    /// equidistant ties do not lock a flock onto one target.
    fn nearest(&mut self, pred: i32, r: i32) -> Result<Option<(i32, i32)>, Trap> {
        let r = r.clamp(0, i32::from(self.ctx.kind.sight));
        let side = (2 * r + 1) as u32;
        self.spend(side * side / SEARCH_DIV)?;
        let (lx, ly) = (lx(self.ctx.cell), ly(self.ctx.cell));
        let want = self.ctx.halo.want(pred);
        for ring in 1..=r {
            let n = 8 * ring;
            let start = (self.draw() % n as u64) as i32;
            for k in 0..n {
                let (dx, dy) = ring_cell(ring, (start + k) % n);
                if self.ctx.halo.test(lx, ly, dx, dy, want) {
                    return Ok(Some((dx, dy)));
                }
            }
        }
        Ok(None)
    }

    /// A free neighbour (`random free`): ring 1 as [`Machine::nearest`]
    /// scans it, with its draw and fuel, but not capped by `sight`: a
    /// neighbour is not a search (`free(east)` is not capped either).
    fn random_free(&mut self) -> Result<Option<(i32, i32)>, Trap> {
        self.spend(9 / SEARCH_DIV)?;
        let (lx, ly) = (lx(self.ctx.cell), ly(self.ctx.cell));
        let start = (self.draw() % 8) as i32;
        for k in 0..8 {
            let (dx, dy) = ring_cell(1, (start + k) % 8);
            if self.ctx.halo.free(lx, ly, dx, dy) {
                return Ok(Some((dx, dy)));
            }
        }
        Ok(None)
    }

    /// The cell with the most scent `ch` in rings `1..=r`: the first such
    /// in scan order, each ring's start rotated by one draw (as `nearest`).
    fn sniff(&mut self, ch: i32, r: i32) -> Result<Option<(i32, i32)>, Trap> {
        let r = r.clamp(0, i32::from(self.ctx.kind.sight));
        let side = (2 * r + 1) as u32;
        self.spend(side * side / SEARCH_DIV)?;
        let ch = usize::try_from(ch).map_err(|_| Trap::BadSense)?;
        if ch >= SCENT_CHANNELS {
            return Err(Trap::BadSense);
        }
        let (lx, ly) = (lx(self.ctx.cell), ly(self.ctx.cell));
        let mut best = (0u8, 0, 0);
        for ring in 1..=r {
            let n = 8 * ring;
            let start = (self.draw() % n as u64) as i32;
            for k in 0..n {
                let (dx, dy) = ring_cell(ring, (start + k) % n);
                let v = self.ctx.halo.scent(lx, ly, dx, dy, ch);
                if v > best.0 {
                    best = (v, dx, dy);
                }
            }
        }
        Ok((best.0 > 0).then_some((best.1, best.2)))
    }

    /// One `for each` step (see [`OpCode::ForEach`]): locals `a..a+5`.
    fn for_each(&mut self, a: u8) -> Result<bool, Trap> {
        if usize::from(a) + usize::from(FOR_EACH_LOCALS) > FRAME_LOCALS {
            return Err(Trap::BadLocal);
        }
        let base = self.local(a)?;
        let cursor = self.locals[base + 2].max(0);
        let pred = self.locals[base + 3];
        let r = self.locals[base + 4].clamp(0, i32::from(self.ctx.kind.sight));
        if cursor == 0 {
            let side = (2 * r + 1) as u32;
            self.spend(side * side / SEARCH_DIV)?;
        }
        let (lx, ly) = (lx(self.ctx.cell), ly(self.ctx.cell));
        let want = self.ctx.halo.want(pred);
        // Cells before ring `ring` (rings from 1): 4 * ring * (ring - 1).
        let mut ring = 1;
        while ring <= r && 4 * ring * (ring + 1) <= cursor {
            ring += 1;
        }
        let mut k = cursor;
        while ring <= r {
            let first = 4 * ring * (ring - 1);
            let n = 8 * ring;
            while k < first + n {
                let (dx, dy) = ring_cell(ring, k - first);
                k += 1;
                if self.ctx.halo.test(lx, ly, dx, dy, want) {
                    self.locals[base] = dx;
                    self.locals[base + 1] = dy;
                    self.locals[base + 2] = k;
                    return Ok(true);
                }
            }
            ring += 1;
        }
        self.locals[base + 2] = k;
        Ok(false)
    }

    fn act(&mut self, a: u8) -> Result<(), Trap> {
        if self.acted {
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
                // Every cell of the halo is within 127 each way. Past that
                // the offset becomes -128, which is outside the halo from any
                // cell too, and Apply answers BLOCKED (not clamped: that
                // would spawn on another cell).
                self.out.dx = i8::try_from(dx).unwrap_or(i8::MIN);
                self.out.dy = i8::try_from(dy).unwrap_or(i8::MIN);
            }
            Action::Take | Action::Give => {
                let amount = self.pop()?;
                let need = self.pop()?;
                let dy = self.pop()?;
                let dx = self.pop()?;
                let need = usize::try_from(need).map_err(|_| Trap::BadNeed)?;
                if need >= self.ctx.kind.needs.len() {
                    return Err(Trap::BadNeed);
                }
                self.out.need = need as u8;
                self.out.amount = amount.max(0);
                self.out.dx = dx.clamp(-127, 127) as i8;
                self.out.dy = dy.clamp(-127, 127) as i8;
            }
            Action::Move | Action::Drink | Action::Eat | Action::Hit | Action::Graze => {
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

    /// Execute from `self.pc` until a halt. With `TRACE`, every executed op
    /// is appended to `trace` (`wmc why`); without it the pushes compile
    /// away and `trace` is never touched.
    fn run<const TRACE: bool>(
        &mut self,
        code: &[Op],
        consts: &[i32],
        subs: &[u32],
        trace: &mut Vec<Step>,
    ) -> Result<(), Trap> {
        use OpCode as O;
        loop {
            let at = self.pc;
            if TRACE {
                self.at = at;
            }
            let op = *code.get(self.pc).ok_or(Trap::BadPc)?;
            self.pc += 1;
            self.spend(1)?;
            self.out.used += 1;
            let halt = 'op: {
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
                            O::Min => x.min(y),
                            _ => x.max(y),
                        };
                        self.push(v)?;
                    }
                    O::Neg => {
                        let x = self.pop()?;
                        self.push(x.wrapping_neg())?;
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
                    O::Call => {
                        if self.depth == FRAMES {
                            return Err(Trap::CallDepth);
                        }
                        let target =
                            *subs.get(op.imm as u16 as usize).ok_or(Trap::BadSub)? as usize;
                        let args = usize::from(op.a);
                        if args > FRAME_LOCALS || self.sp < args {
                            return Err(Trap::StackUnderflow);
                        }
                        // Frame d's locals start at d * FRAME_LOCALS (the
                        // rule's at 0), and LOCALS holds FRAMES + 1 frames.
                        let new_base = self.base + FRAME_LOCALS;
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
                            break 'op true;
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
                    O::Nearest | O::Sniff => {
                        let r = self.pop()?;
                        let what = self.pop()?;
                        let i = self.local(op.a)?;
                        if usize::from(op.a) + 1 >= FRAME_LOCALS {
                            return Err(Trap::BadLocal);
                        }
                        let found = if op.code == O::Nearest {
                            self.nearest(what, r)?
                        } else {
                            self.sniff(what, r)?
                        };
                        match found {
                            Some((dx, dy)) => {
                                self.locals[i] = dx;
                                self.locals[i + 1] = dy;
                                self.push(1)?;
                            }
                            None => self.push(0)?,
                        }
                    }
                    O::RandomFree => {
                        let i = self.local(op.a)?;
                        if usize::from(op.a) + 1 >= FRAME_LOCALS {
                            return Err(Trap::BadLocal);
                        }
                        match self.random_free()? {
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
                        let (dx, dy) = match i {
                            1..=8 => DIRS8[(i - 1) as usize],
                            _ => (0, 0),
                        };
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
                            break 'op true;
                        }
                    }
                    O::Halt => break 'op true,
                    O::SetSignal => {
                        let v = self.pop()?;
                        self.out.signal =
                            Some(v.clamp(i32::from(i16::MIN), i32::from(i16::MAX)) as i16);
                    }
                    O::LookAt | O::SignalAt => {
                        let dy = self.pop()?;
                        let dx = self.pop()?;
                        let (lx, ly) = (lx(self.ctx.cell), ly(self.ctx.cell));
                        let v = match self.ctx.halo.row_at(lx, ly, dx, dy) {
                            None => 0,
                            Some(r) if op.code == O::LookAt => i32::from(r.look),
                            Some(r) => i32::from(r.signal),
                        };
                        self.push(v)?;
                    }
                    O::Pack => {
                        let lo = self.pop()?;
                        let hi = self.pop()?;
                        self.push(hi.wrapping_mul(256).wrapping_add(lo & 0xFF))?;
                    }
                    O::Hi => {
                        let v = self.pop()?;
                        self.push(v >> 8)?;
                    }
                    O::Lo => {
                        let v = self.pop()?;
                        self.push(i32::from(v as u8 as i8))?;
                    }
                    O::ForEach => {
                        let found = self.for_each(op.a)?;
                        self.push(i32::from(found))?;
                    }
                    O::SpawnWith => {
                        let b = self.pop()?;
                        let a = self.pop()?;
                        self.out.with = [a, b];
                    }
                    O::Mark => {
                        let v = self.pop()?;
                        if usize::from(op.a) >= SCENT_CHANNELS {
                            return Err(Trap::BadSense);
                        }
                        let m = &mut self.out.mark[usize::from(op.a)];
                        *m = m.saturating_add(v.clamp(0, 255) as u8);
                    }
                    O::ScentAt => {
                        let dy = self.pop()?;
                        let dx = self.pop()?;
                        if usize::from(op.a) >= SCENT_CHANNELS {
                            return Err(Trap::BadSense);
                        }
                        let (lx, ly) = (lx(self.ctx.cell), ly(self.ctx.cell));
                        let v = self.ctx.halo.scent(lx, ly, dx, dy, usize::from(op.a));
                        self.push(i32::from(v))?;
                    }
                }
                false
            };
            if TRACE {
                trace.push(Step {
                    pc: at as u32,
                    op,
                    top: self.sp.checked_sub(1).map(|i| self.stack[i]),
                    fuel: self.fuel,
                });
            }
            if halt {
                return Ok(());
            }
        }
    }
}

#[inline]
fn jump(pc: usize, imm: i16) -> Result<usize, Trap> {
    usize::try_from(pc as i64 + i64::from(imm)).map_err(|_| Trap::BadPc)
}

#[inline]
pub(crate) fn lx(cell: usize) -> i32 {
    (cell as i32) & (CHUNK_SIZE - 1)
}

#[inline]
pub(crate) fn ly(cell: usize) -> i32 {
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
    think_with::<false>(program, ctx, mind, &mut Vec::new())
}

/// [`think`], with every executed op appended to `trace` (`wmc why`). Same
/// outcome, same writes to `mind`.
pub fn think_traced(
    program: &super::Kinds,
    ctx: Ctx<'_>,
    mind: &mut ActorMind,
    trace: &mut Vec<Step>,
) -> Outcome {
    think_with::<true>(program, ctx, mind, trace)
}

#[inline(always)]
fn think_with<const TRACE: bool>(
    program: &super::Kinds,
    ctx: Ctx<'_>,
    mind: &mut ActorMind,
    trace: &mut Vec<Step>,
) -> Outcome {
    let mut m = Machine {
        stack: [0; STACK],
        sp: 0,
        locals: [0; LOCALS],
        base: 0,
        frames: [(0, 0); FRAMES],
        depth: 0,
        pc: ctx.kind.entry as usize,
        at: 0,
        fuel: ctx.kind.fuel,
        draws: 0,
        out: Outcome::default(),
        acted: false,
        ctx,
        mind,
    };
    if let Err(trap) = m.run::<TRACE>(&program.code, &program.consts, &program.subs, trace) {
        // Needs and mem written before the trap stay (RULES.md §13).
        // The op that trapped (none for a bad pc), so `wmc why` shows where.
        if TRACE && let Some(&op) = program.code.get(m.at) {
            trace.push(Step {
                pc: m.at as u32,
                op,
                top: m.sp.checked_sub(1).map(|i| m.stack[i]),
                fuel: m.fuel,
            });
        }
        m.out.trap = Some(trap);
        m.out.action = Action::Idle;
        m.out.next = None;
        m.out.look = None;
        m.out.signal = None;
        m.out.mark = [0; SCENT_CHANNELS];
    }
    m.out
}

/// Decay a mind's consumable needs by the ticks since its last think and
/// stamp it. Returns `true` if a vital need is empty (the actor dies).
pub fn decay(kind: &KindDef, mind: &mut ActorMind, tick: u64) -> bool {
    let mut dead = false;
    for (i, need) in kind.needs.iter().enumerate().take(NEED_SLOTS) {
        mind.needs[i] = need_now(mind.needs[i], need.decays, tick, mind.last_think);
        dead |= need.vital && mind.needs[i] <= 0;
    }
    mind.last_think = tick as u32;
    dead
}

/// A need's value `v` as it stands at `tick`: decayed by the ticks since
/// `last_think` if it decays, never below 0. What [`decay`] writes.
pub fn need_now(v: i32, decays: bool, tick: u64, last_think: u32) -> i32 {
    if decays {
        let elapsed = i64::from((tick as u32).wrapping_sub(last_think));
        (i64::from(v) - elapsed).max(0) as i32
    } else {
        v
    }
}

/// Where `(dx, dy)` from local cell `cell` lands: the chunk offset
/// (`-1..=1` each way, or beyond) and the local index there.
#[inline]
pub fn offset_cell(cell: usize, dx: i8, dy: i8) -> ((i32, i32), usize) {
    let (x, y) = (lx(cell) + i32::from(dx), ly(cell) + i32::from(dy));
    let local = ((y & (CHUNK_SIZE - 1)) * CHUNK_SIZE + (x & (CHUNK_SIZE - 1))) as usize;
    ((x >> CHUNK_BITS, y >> CHUNK_BITS), local)
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
    use crate::time::TICKS_PER_DAY;
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
            color: 0,
            cover: false,
            parent: None,
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
        run_kinds(
            &Kinds::from_parts(vec![kind(needs, 0)], code, consts, vec![]),
            mind,
        )
    }

    /// One think of kind 0 at (11, 10) on [`stage`].
    fn run_kinds(kinds: &Kinds, mind: &mut ActorMind) -> Outcome {
        run_at(kinds, mind, 1000)
    }

    /// [`run_kinds`] at `tick`.
    fn run_at(kinds: &Kinds, mind: &mut ActorMind, tick: u64) -> Outcome {
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
            family_end: &[],
        };
        let ctx = Ctx {
            halo: &halo,
            kind: &kinds.defs[0],
            cell: 10 * 64 + 11,
            pos: Pos::new(11, 10),
            tick,
            rng: rng_base(1, tick, 7),
            look: 0,
            signal: 0,
        };
        think(kinds, ctx, mind)
    }

    fn mind() -> ActorMind {
        ActorMind::zeroed()
    }

    #[test]
    fn sense_numbering_is_dense() {
        for (i, s) in Sense::ALL.into_iter().enumerate() {
            assert_eq!(s as usize, i);
            assert_eq!(Sense::from_u8(i as u8), Some(s));
        }
        assert_eq!(Sense::from_u8(Sense::ALL.len() as u8), None);
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
        a.push(3).push(4).op(OpCode::Lt).set_mem(6);
        a.push(4).push(3).op(OpCode::Ge).set_mem(7);
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
        // An explicit `idle` is the think's action too (RULES.md §2).
        let mut a = Asm::new();
        a.act(Action::Idle).push(1).push(0).act(Action::Move).halt();
        let out = run(a.finish(), vec![], vec![], &mut mind());
        assert_eq!(out.trap, Some(Trap::SecondAction));
        assert_eq!(out.action, Action::Idle);
    }

    /// RULES.md §13: a trap keeps the mem and need writes made before it
    /// and drops the action, `next` and the effects.
    #[test]
    fn a_trap_keeps_earlier_writes_and_drops_effects() {
        let mut a = Asm::new();
        a.push(5).set_mem(0).next(0).push(3).op(OpCode::SetLook);
        a.act(Action::Die).act(Action::Die).halt();
        let mut m = mind();
        let out = run(a.finish(), vec![], vec![], &mut m);
        assert_eq!(out.trap, Some(Trap::SecondAction));
        assert_eq!(m.mem[0], 5);
        assert_eq!((out.action, out.next, out.look), (Action::Idle, None, None));
    }

    /// A traced think records the op that trapped too (`wmc why` shows
    /// where); a fuel trap's last step has no fuel left.
    #[test]
    fn the_trapping_op_is_traced() {
        let (cells, actors) = stage();
        let mut halo = Halo {
            chunks: [None; 9],
            tags: &[],
            family_end: &[],
        };
        halo.chunks[4] = Some((&cells, &actors));
        let traced = |a: Asm| {
            let kinds = Kinds::from_parts(vec![kind(vec![], 0)], a.finish(), vec![], vec![]);
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
            let mut trace = Vec::new();
            let out = think_traced(&kinds, ctx, &mut mind(), &mut trace);
            (out.trap, *trace.last().unwrap())
        };
        let mut a = Asm::new();
        a.act(Action::Die).act(Action::Die).halt();
        let (trap, last) = traced(a);
        assert_eq!(trap, Some(Trap::SecondAction));
        assert_eq!((last.pc, last.op.code), (1, OpCode::Act));
        let mut a = Asm::new();
        let top = a.label();
        a.bind(top);
        a.mem(0).push(1).op(OpCode::Add).set_mem(0).jmp(top);
        let (trap, last) = traced(a);
        assert_eq!(trap, Some(Trap::Fuel));
        assert_eq!(last.fuel, 0);
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
    fn day_saturates_past_i32_max_days() {
        let mut a = Asm::new();
        a.sense(Sense::Day).set_mem(0).halt();
        let kinds = Kinds::from_parts(vec![kind(vec![], 0)], a.finish(), vec![], vec![]);
        // The latest tick a save may hold is 2^63 - 1: day 4.3e14.
        for (tick, day) in [
            (3 * TICKS_PER_DAY + 5, 3),
            (u64::from(i32::MAX.unsigned_abs()) * TICKS_PER_DAY, i32::MAX),
            ((1 << 32) * TICKS_PER_DAY, i32::MAX),
            (u64::MAX / 2, i32::MAX),
        ] {
            let mut m = mind();
            assert_eq!(run_at(&kinds, &mut m, tick).trap, None);
            assert_eq!(m.mem[0], day, "tick {tick}");
        }
    }

    #[test]
    fn needs_are_clamped_and_senses_read_the_world() {
        let mut a = Asm::new();
        a.push(500).set_need(0); // clamped to max 100
        a.push(-5).set_need(1); // clamped to 0
        a.sense(Sense::Light).set_mem(0);
        a.sense(Sense::X).set_mem(1);
        a.sense(Sense::Y).set_mem(2);
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
        let kinds = Kinds::from_parts(vec![kind(vec![], 0)], a.finish(), vec![], vec![twice]);
        let mut m = mind();
        let out = run_kinds(&kinds, &mut m);
        assert_eq!(out.trap, None);
        assert_eq!(m.mem[0], 42);
        // Unbounded recursion trips the depth limit, not the stack or the
        // locals: f(d) writes d to mem1 before it calls f(d + 1), so mem1
        // is the deepest frame that ran.
        let mut a = Asm::new();
        a.push(1).call(0, 1).halt();
        let f = a.here();
        a.load(0).set_mem(1);
        a.load(0).push(1).op(OpCode::Add).call(0, 1).ret(false);
        let kinds = Kinds::from_parts(vec![kind(vec![], 0)], a.finish(), vec![], vec![f]);
        assert_eq!(run_kinds(&kinds, &mut m).trap, Some(Trap::CallDepth));
        assert_eq!(m.mem[1], FRAMES as i32);
    }

    /// RULES.md §12 and §18: calls nest up to 8 deep. `sum(n)` runs n + 1
    /// nested frames and reads its own `n` after the calls above it return,
    /// so every frame needs locals of its own.
    #[test]
    fn calls_nest_eight_deep_and_the_ninth_traps() {
        // entry: mem0 = sum(n); halt.
        // sum(n): if n == 0 { return 0 }  return sum(n - 1) + n
        let sum = |n: i32| {
            let mut a = Asm::new();
            a.push(n).call(0, 1).set_mem(0).halt();
            let sub = a.here();
            let zero = a.label();
            a.load(0).jz(zero);
            a.load(0).push(1).op(OpCode::Sub).call(0, 1);
            a.load(0).op(OpCode::Add).ret(true);
            a.bind(zero);
            a.push(0).ret(true);
            let kinds = Kinds::from_parts(vec![kind(vec![], 0)], a.finish(), vec![], vec![sub]);
            let mut m = mind();
            let out = run_kinds(&kinds, &mut m);
            (out.trap, m.mem[0])
        };
        assert_eq!(sum(2), (None, 3), "3 nested calls");
        assert_eq!(sum(7), (None, 28), "8 nested calls");
        assert_eq!(sum(8), (Some(Trap::CallDepth), 0), "a 9th traps");
    }

    /// A target a rule computes can be anywhere in i32: far ones read as
    /// unloaded (rock, nobody), a heading outside 1..=8 is no step.
    #[test]
    fn far_targets_and_bad_headings_do_not_panic() {
        let mut a = Asm::new();
        a.push_k(0).push(0).op(OpCode::FreeAt).set_mem(0);
        a.push_k(0)
            .push(0)
            .push(pred::feature(1))
            .op(OpCode::IsAt)
            .set_mem(1);
        a.push(0).push_k(0).op(OpCode::LookAt).set_mem(2);
        a.push(0).push_k(0).op(OpCode::SignalAt).set_mem(3);
        a.push_k(0).push(0).scent_at(0).set_mem(4);
        a.push_k(1).op(OpCode::DirOf).set_mem(6).set_mem(5);
        a.push(9).op(OpCode::DirOf).set_mem(8).set_mem(7);
        a.push_k(0).op(OpCode::DirOf).set_mem(10).set_mem(9);
        a.halt();
        let mut m = mind();
        m.mem = [7; MEM_SLOTS];
        let out = run(a.finish(), vec![i32::MAX, i32::MIN], vec![], &mut m);
        assert_eq!(out.trap, None);
        assert_eq!(&m.mem[..11], &[0, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
    }

    /// The local-index helpers agree with `ChunkCoord::cell` and
    /// `Pos::split`, whatever `CHUNK_BITS` is.
    #[test]
    fn local_index_helpers_follow_chunk_bits() {
        use crate::stage::ChunkCoord;
        let c0 = ChunkCoord::new(0, 0);
        for i in 0..CHUNK_CELLS {
            assert_eq!(c0.cell(i), Pos::new(lx(i), ly(i)), "cell {i}");
            for (dx, dy) in [(-70i8, -70i8), (-1, 0), (0, 1), (1, 1), (37, -5), (70, 70)] {
                let (c, local) = Pos::new(lx(i) + i32::from(dx), ly(i) + i32::from(dy)).split();
                assert_eq!(offset_cell(i, dx, dy), ((c.x, c.y), local), "{i} {dx} {dy}");
            }
        }
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
            family_end: &[],
        };
        halo.chunks[4] = Some((&cells, &actors));
        halo.chunks[5] = Some((&cells, &actors)); // east neighbour
        // From local (63, 5), +1 in x lands in the east chunk at (0, 5).
        let (_, _, i) = halo.at(63, 5, 1, 0).unwrap();
        assert_eq!(i, 5 * 64);
        assert!(halo.at(63, 5, 1, -6).is_none()); // north-east: not loaded
        assert!(halo.at(0, 0, -1, 0).is_none()); // west: not loaded
        assert!(halo.at(63, 5, i32::MAX, 0).is_none()); // far: outside the halo
        assert!(halo.at(5, 63, 0, i32::MAX).is_none());
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

    /// A kind predicate matches the kind's family, `only` the kind alone,
    /// for occupants and ground cover, with and without a look.
    #[test]
    fn family_and_only_matching() {
        use crate::actors::ActorPub;
        // animal 0 > bird 1 > hen 2; plant 3.
        let family_end = [3u16, 3, 3, 4];
        let mut cells = ChunkCells::default();
        let mut actors = ChunkActors::default();
        cells.occupant[5] = ActorId::pack(2, 0); // a hen
        actors.rows.push(ActorPub {
            cell: 5,
            kind: 2,
            look: 7,
            ..ActorPub::zeroed()
        });
        cells.cover[9] = ActorId::pack(3, 1); // a plant, as cover
        actors.rows.push(ActorPub {
            cell: 9,
            kind: 3,
            ..ActorPub::zeroed()
        });
        let m = |p: i32, i: usize| Want::of(p, &family_end).test(&cells, &actors, i, &[]);
        assert!(
            m(0, 5) && m(1, 5) && m(2, 5),
            "a hen is an animal, a bird, a hen"
        );
        assert!(!m(3, 5));
        assert!(!m(pred::ONLY, 5) && !m(pred::ONLY + 1, 5) && m(pred::ONLY + 2, 5));
        assert!(m(pred::kind_look(0, 7), 5) && !m(pred::kind_look(0, 6), 5));
        assert!(!m(pred::kind_look(0, 7) + pred::ONLY, 5));
        assert!(m(pred::kind_look(2, 7) + pred::ONLY, 5));
        assert!(m(3, 9) && m(pred::ONLY + 3, 9) && !m(0, 9));
        // No family table: every kind is its own family.
        assert!(!Want::of(0, &[]).test(&cells, &actors, 5, &[]));
        assert!(Want::of(2, &[]).test(&cells, &actors, 5, &[]));
    }
}
