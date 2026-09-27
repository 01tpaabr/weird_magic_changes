//! The rules compiler: text -> [`Kinds`] (`docs/ACTORS.md` §5).
//!
//! Passes: a lexer per file (tokens with line and column); a
//! recursive-descent parser that adds each file's kinds, traits, subs,
//! consts and tags to one item list; inheritance resolution (parents
//! linearized; declarations, needs, mems, states and rule lists merged); a
//! code generator that drives [`Asm`]; and the author lint (`lint`). Files
//! go in sorted-name order, never directory order, and kinds are numbered
//! in pre-order over the inheritance forest (roots in file then declaration
//! order, each kind's children right after it), so two processes agree on
//! every kind id and a family is one id range. Every error carries
//! `file:line:col`; the sim never runs a program that did not compile.
//!
//! A file sub is shared by every kind, so inside it a name is a parameter,
//! a local or a const, never a need or a mem slot. A member sub (a sub
//! inside a trait or kind) is compiled per kind and sees that kind's needs,
//! mems and states. A sub that `return`s a
//! value anywhere is a function (usable in expressions), otherwise a
//! procedure (a statement). Targets are `(dx, dy)` pairs: two stack values
//! in flight, two locals at rest.

use std::cell::RefCell;
use std::collections::HashMap;
use std::fmt;

use super::asm::{Asm, Label};
use super::vm::{
    Action, FOR_EACH_LOCALS, FRAME_LOCALS, FRAMES, OpCode, STACK, Sense, pred, result,
};
use super::{DEFAULT_COLOR, DebugInfo, Diagnostic, KindDef, Kinds, Level, NeedDef, RuleInfo};
use crate::actors::{MEM_SLOTS, NEED_SLOTS};
use crate::stage::{Feature, Ground, SCENT_CHANNELS};
use crate::time::{days, hours, minutes};

mod lint;

/// A compile error with its position.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompileError {
    pub file: String,
    pub line: u32,
    pub col: u32,
    pub msg: String,
}

impl fmt::Display for CompileError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}:{}: {}", self.file, self.line, self.col, self.msg)
    }
}

impl std::error::Error for CompileError {}

type Result<T> = std::result::Result<T, CompileError>;

/// Compile one file's text.
pub fn compile(file: &str, text: &str) -> Result<Kinds> {
    compile_files(&[(file, text)])
}

/// Compile several files as one rule set, in the order given (callers sort
/// by name). Kind and sub names are global across files.
pub fn compile_files(files: &[(&str, &str)]) -> Result<Kinds> {
    if let Some((name, _)) = files.get(MAX_FILES) {
        return Err(CompileError {
            file: name.to_string(),
            line: 1,
            col: 1,
            msg: format!("at most {MAX_FILES} rules files in a rule set"),
        });
    }
    let mut items = Items::default();
    for (name, text) in files {
        let tokens = Lexer::new(name, text).lex()?;
        let mut p = Parser {
            file: name,
            tokens,
            at: 0,
            depth: 0,
            deepest: 0,
        };
        p.file(&mut items)?;
    }
    Gen::new(&items, files).generate()
}

/// Compile rule packs as one rule set. A pack is a directory (its `*.rules`
/// files, in sorted file-name order; a directory with none is an error) or
/// a single file; packs go in the order given, with one namespace across
/// all of them (a name declared in two packs is an error naming both). A
/// pack given twice loads once.
/// With more than one pack, a directory's file is named `pack/file.rules`
/// in positions, `pack` being the directory's own name; two files that
/// would share a name take parent directories until they don't. The
/// packs' absolute paths go in `debug.packs`: a save remembers them.
pub fn compile_packs(
    packs: &[&std::path::Path],
) -> std::result::Result<Kinds, Box<dyn std::error::Error>> {
    // Each pack once, where first given: a scenario's `rules` line and the
    // same `--rules` are one pack.
    let mut full_paths: Vec<(&std::path::Path, std::path::PathBuf)> = Vec::new();
    for pack in packs {
        let full = std::fs::canonicalize(pack).map_err(|e| format!("{}: {e}", pack.display()))?;
        if !full_paths.iter().any(|(_, f)| *f == full) {
            full_paths.push((pack, full));
        }
    }
    // Every file once, where first loaded, with how many trailing path
    // components label it.
    let mut files: Vec<(std::path::PathBuf, usize)> = Vec::new();
    for (pack, full) in &full_paths {
        if full.is_dir() {
            let mut v: Vec<_> = std::fs::read_dir(full)
                .map_err(|e| format!("{}: {e}", pack.display()))?
                .filter_map(|e| e.ok().map(|e| e.path()))
                .filter(|p| p.extension().is_some_and(|x| x == "rules"))
                .collect();
            if v.is_empty() {
                return Err(format!("{}: holds no .rules files", pack.display()).into());
            }
            v.sort();
            let k = if full_paths.len() > 1 { 2 } else { 1 };
            for p in v {
                if !files.iter().any(|(q, _)| *q == p) {
                    files.push((p, k));
                }
            }
        } else if !files.iter().any(|(q, _)| q == full) {
            files.push((full.clone(), 1));
        }
    }
    // A label names one file: while two are equal, each takes one more
    // parent directory (`one/pk/a.rules`, `two/pk/a.rules`).
    let parts = |p: &std::path::Path| -> Vec<String> {
        p.components()
            .filter_map(|c| match c {
                std::path::Component::Normal(s) => Some(s.to_string_lossy().into_owned()),
                _ => None,
            })
            .collect()
    };
    let label = |p: &std::path::Path, k: usize| {
        let c = parts(p);
        c[c.len().saturating_sub(k)..].join("/")
    };
    loop {
        let labels: Vec<String> = files.iter().map(|(p, k)| label(p, *k)).collect();
        let mut grew = false;
        for (i, (p, k)) in files.iter_mut().enumerate() {
            if labels.iter().filter(|l| **l == labels[i]).count() > 1 && *k < parts(p).len() {
                *k += 1;
                grew = true;
            }
        }
        if !grew {
            break;
        }
    }
    let mut texts: Vec<(String, String)> = Vec::with_capacity(files.len());
    for (p, k) in &files {
        let text = std::fs::read_to_string(p).map_err(|e| format!("{}: {e}", p.display()))?;
        texts.push((label(p, *k), text));
    }
    let files: Vec<(&str, &str)> = texts
        .iter()
        .map(|(n, t)| (n.as_str(), t.as_str()))
        .collect();
    let mut kinds = compile_files(&files)?;
    kinds.debug.packs = full_paths
        .iter()
        .map(|(_, f)| f.to_string_lossy().into_owned())
        .collect();
    Ok(kinds)
}

// ---- lexer ----------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
enum Tok {
    Int(i32),
    /// A time literal, already in ticks.
    Time(i32),
    Str(String),
    Name(String),
    /// Punctuation and operators.
    Sym(&'static str),
    Eof,
}

#[derive(Debug, Clone)]
struct Token {
    tok: Tok,
    line: u32,
    col: u32,
}

const SYMS: [&str; 23] = [
    "=>", "==", "!=", "<=", ">=", "+=", "-=", "{", "}", "(", ")", ",", ":", "=", "<", ">", "+",
    "-", "*", "/", "%", ";", ".",
];

struct Lexer<'a> {
    file: &'a str,
    text: &'a str,
    src: &'a [u8],
    at: usize,
    line: u32,
    col: u32,
}

impl<'a> Lexer<'a> {
    fn new(file: &'a str, text: &'a str) -> Self {
        Self {
            file,
            text,
            src: text.as_bytes(),
            at: 0,
            line: 1,
            col: 1,
        }
    }

    fn err(&self, msg: impl Into<String>) -> CompileError {
        CompileError {
            file: self.file.to_string(),
            line: self.line,
            col: self.col,
            msg: msg.into(),
        }
    }

    /// The byte `k` ahead, 0 past the end (a real NUL byte is also 0:
    /// only [`Self::end`] says where the file ends).
    fn peek(&self, k: usize) -> u8 {
        self.src.get(self.at + k).copied().unwrap_or(0)
    }

    fn end(&self) -> bool {
        self.at >= self.src.len()
    }

    /// Columns count characters: a UTF-8 continuation byte adds none.
    fn bump(&mut self) -> u8 {
        let b = self.peek(0);
        self.at += 1;
        if b == b'\n' {
            self.line += 1;
            self.col = 1;
        } else if b & 0xC0 != 0x80 {
            self.col += 1;
        }
        b
    }

    fn lex(mut self) -> Result<Vec<Token>> {
        let mut out = Vec::new();
        loop {
            // Whitespace and comments.
            loop {
                match self.peek(0) {
                    b' ' | b'\t' | b'\r' | b'\n' => {
                        self.bump();
                    }
                    b'#' => {
                        while !self.end() && self.peek(0) != b'\n' {
                            self.bump();
                        }
                    }
                    _ => break,
                }
            }
            let (line, col) = (self.line, self.col);
            let b = self.peek(0);
            let tok = if self.end() {
                Tok::Eof
            } else if b.is_ascii_digit() {
                let start = self.at;
                let mut v: i64 = 0;
                while self.peek(0).is_ascii_digit() {
                    v = v * 10 + i64::from(self.bump() - b'0');
                    if v > i64::from(i32::MAX) {
                        return Err(self.err("number too large"));
                    }
                }
                let mut unit = String::new();
                while self.peek(0).is_ascii_alphabetic() {
                    unit.push(char::from(self.bump()));
                }
                // `5_m` or `3h2` would lex as two tokens and may parse.
                if self.peek(0).is_ascii_digit() || self.peek(0) == b'_' {
                    let mut s = self.text[start..self.at].to_string();
                    while self.peek(0).is_ascii_alphanumeric() || self.peek(0) == b'_' {
                        s.push(char::from(self.bump()));
                    }
                    return Err(CompileError {
                        file: self.file.to_string(),
                        line,
                        col,
                        msg: format!("`{s}`: a number runs into a name"),
                    });
                }
                let ticks = |t: u64| i32::try_from(t).ok();
                match unit.as_str() {
                    "" => Tok::Int(i32::try_from(v).expect("checked against i32::MAX above")),
                    "min" => Tok::Time(
                        ticks(minutes(v as u64)).ok_or_else(|| self.err("time too long"))?,
                    ),
                    "h" => {
                        Tok::Time(ticks(hours(v as u64)).ok_or_else(|| self.err("time too long"))?)
                    }
                    "d" => {
                        Tok::Time(ticks(days(v as u64)).ok_or_else(|| self.err("time too long"))?)
                    }
                    u => return Err(self.err(format!("unknown unit `{u}` (min, h, d)"))),
                }
            } else if b.is_ascii_alphabetic() || b == b'_' {
                let mut s = String::new();
                while self.peek(0).is_ascii_alphanumeric() || self.peek(0) == b'_' {
                    s.push(char::from(self.bump()));
                }
                Tok::Name(s)
            } else if b == b'"' {
                self.bump();
                let mut s = String::new();
                loop {
                    match self.peek(0) {
                        b'"' => {
                            self.bump();
                            break;
                        }
                        _ if self.end() => return Err(self.err("unterminated string")),
                        b'\n' => return Err(self.err("unterminated string")),
                        _ => s.push(char::from(self.bump())),
                    }
                }
                Tok::Str(s)
            } else {
                let two = [b, self.peek(1)];
                let sym = SYMS
                    .iter()
                    .find(|s| s.len() == 2 && s.as_bytes() == two)
                    .or_else(|| SYMS.iter().find(|s| s.len() == 1 && s.as_bytes()[0] == b))
                    .copied()
                    .ok_or_else(|| {
                        let c = self.text.get(self.at..).and_then(|t| t.chars().next());
                        let c = c.unwrap_or('?');
                        self.err(format!("unexpected character `{}`", c.escape_debug()))
                    })?;
                for _ in 0..sym.len() {
                    self.bump();
                }
                Tok::Sym(sym)
            };
            let eof = tok == Tok::Eof;
            out.push(Token { tok, line, col });
            if eof {
                return Ok(out);
            }
        }
    }
}

// ---- AST ------------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Ty {
    Int,
    Target,
    Pred,
}

impl Ty {
    /// Stack values / locals it occupies.
    fn width(self) -> u8 {
        match self {
            Ty::Target => 2,
            Ty::Int | Ty::Pred => 1,
        }
    }
}

#[derive(Debug, Clone)]
struct Pos {
    file: String,
    line: u32,
    col: u32,
}

/// A `kind` or a `trait`, as written. A trait has parameters and never a
/// glyph or colour; a kind has no parameters. Either may extend
/// others (`docs/ACTORS.md` §5, traits and inheritance).
#[derive(Debug, Clone)]
struct ItemAst {
    at: Pos,
    name: String,
    is_trait: bool,
    /// Trait parameters: constants inside the trait, bound by `extends`.
    params: Vec<(String, Pos)>,
    parents: Vec<ParentRef>,
    decls: Decls,
    /// Member subs: they see this item's needs, mems and states.
    members: Vec<SubAst>,
    /// Reflexes: scanned first on every think, whatever the state.
    rules: Vec<RuleItem>,
    states: Vec<StateAst>,
}

/// `extends NAME` or `extends NAME(args)`.
#[derive(Debug, Clone)]
struct ParentRef {
    name: String,
    args: Vec<Expr>,
    at: Pos,
}

/// What an item declares itself. `None` or empty: not declared here, so an
/// ancestor's value, else the default, applies. Numbers are constant
/// expressions, folded per instance (they may use trait parameters).
#[derive(Debug, Clone, Default)]
struct Decls {
    glyph: Option<u8>,
    color: Option<u32>,
    cover: bool,
    cadence: Option<(Expr, Pos)>,
    sight: Option<(Expr, Pos)>,
    fuel: Option<(Expr, Pos)>,
    food: Option<(Expr, Pos)>,
    bite: Option<(Expr, Pos)>,
    tags: Vec<String>,
    needs: Vec<NeedAst>,
    mems: Vec<(String, Pos)>,
}

/// `need NAME max M [decay 0] [vital]`.
#[derive(Debug, Clone)]
struct NeedAst {
    name: String,
    max: Expr,
    decays: bool,
    vital: bool,
    at: Pos,
}

/// One entry of a rule list: a rule, or `inherit [NAME]`, which splices
/// ancestors' rules at that point.
#[derive(Debug, Clone)]
enum RuleItem {
    When(Box<Rule>),
    Inherit(Option<String>, Pos),
}

/// Everything the files declare, in file order then declaration order.
#[derive(Debug, Default)]
struct Items {
    /// Kinds and traits, in file then declaration order.
    items: Vec<ItemAst>,
    subs: Vec<SubAst>,
    consts: Vec<ConstAst>,
}

/// `const NAME = expr`: a file-scope integer, folded at compile time.
#[derive(Debug, Clone)]
struct ConstAst {
    at: Pos,
    name: String,
    value: Expr,
}

/// `state NAME { rule* }`: the rules scanned after the reflexes while the
/// actor is in this state.
#[derive(Debug, Clone)]
struct StateAst {
    at: Pos,
    name: String,
    rules: Vec<RuleItem>,
}

#[derive(Debug, Clone)]
struct SubAst {
    at: Pos,
    name: String,
    params: Vec<(String, Ty)>,
    body: Vec<Stmt>,
    /// Has a `return expr` somewhere: a function.
    returns: bool,
}

#[derive(Debug, Clone)]
struct Rule {
    /// Where its `when` is, and the line of its `=>`.
    at: Pos,
    arrow_line: u32,
    cond: Cond,
    body: Vec<Stmt>,
}

#[derive(Debug, Clone)]
enum Cond {
    Expr(Expr),
    Nearest {
        pred: Pred,
        r: Expr,
        bind: String,
        at: Pos,
    },
    /// `sniff ch within r as v`: the strongest cell of scent `ch`.
    Sniff {
        ch: String,
        r: Expr,
        bind: String,
        at: Pos,
    },
    And(Box<Cond>, Box<Cond>),
    Or(Box<Cond>, Box<Cond>),
    Not(Box<Cond>),
}

#[derive(Debug, Clone)]
enum Target {
    Named(String, Pos),
    Here,
    /// The lowest-key actor that bit this one since its last think.
    Attacker,
    /// `dir(h)`: the unit step of direction `h` (1..=8 clockwise from north).
    Heading(Box<Expr>),
    Dir(i32, i32),
    Toward(Box<Target>),
    Away(Box<Target>),
    At(Box<Expr>, Box<Expr>),
    RandomFree,
}

/// A call argument: typed by the callee's parameter at codegen.
#[derive(Debug, Clone)]
enum Arg {
    Expr(Expr),
    Target(Target),
    /// `kind:look`, only a pred.
    Pred(Pred),
    /// A bare name: an int, a target or a pred, whatever the parameter says.
    Name(String, Pos),
}

#[derive(Debug, Clone)]
enum Stmt {
    Set {
        name: String,
        at: Pos,
        value: Expr,
    },
    Assign {
        name: String,
        at: Pos,
        op: OpCode,
        value: Expr,
    },
    Let {
        name: String,
        at: Pos,
        value: Expr,
    },
    If {
        cond: Cond,
        then: Vec<Stmt>,
        els: Vec<Stmt>,
    },
    While {
        cond: Cond,
        body: Vec<Stmt>,
    },
    Repeat {
        count: Expr,
        body: Vec<Stmt>,
    },
    Choose(Vec<(Expr, Vec<Stmt>)>),
    Call {
        name: String,
        args: Vec<Arg>,
        at: Pos,
    },
    Return {
        value: Option<Expr>,
        at: Pos,
    },
    Idle(Pos),
    Die(Pos),
    Become {
        kind: String,
        at: Pos,
    },
    Spawn {
        kind: String,
        at: Target,
        pos: Pos,
        /// `with (m = a, n = b)`: the child's memory slots it sets (at
        /// most two), by name.
        with: Vec<(String, Expr, Pos)>,
    },
    /// `take t NEED amount` / `give t NEED amount`.
    Transfer {
        give: bool,
        target: Target,
        need: String,
        amount: Expr,
        at: Pos,
    },
    Move(Target, Pos),
    Drink(Target, Pos),
    Eat(Target, Pos),
    Hit(Target, Pos),
    Graze(Target, Pos),
    /// `look = v`: an effect, not an action.
    Look(Expr),
    /// `signal = v`: an effect, not an action.
    Signal(Expr),
    /// `mark ch v`: an effect, adds `v` of scent `ch` to the actor's cell.
    Mark(String, Expr, Pos),
    /// `next NAME`: this state for the following think; ends the think
    /// like an action.
    Next(String, Pos),
    /// `for each pred within r as v { body }`: `body` once per matching cell,
    /// in ring order, with `v` bound to it.
    ForEach {
        pred: Pred,
        r: Expr,
        bind: String,
        body: Vec<Stmt>,
        at: Pos,
    },
}

#[derive(Debug, Clone)]
enum Pred {
    /// A kind (its family, or exactly it with `only`), a tag, or a sub's
    /// pred parameter.
    Kind(String, bool, Pos),
    /// `kind:look`: that kind (family, or `only`) showing that look byte.
    KindLook(String, u8, bool, Pos),
    Ground(Ground),
    Feature(Feature),
    Free,
    Bare,
}

/// The words that are a predicate by themselves: the ground, a feature,
/// `free`, `bare`.
fn pred_word(n: &str) -> Option<Pred> {
    Some(match n {
        "free" => Pred::Free,
        "bare" => Pred::Bare,
        "water" => Pred::Ground(Ground::Water),
        "soil" => Pred::Ground(Ground::Soil),
        "rock" => Pred::Feature(Feature::Rock),
        _ => return None,
    })
}

#[derive(Debug, Clone)]
enum Expr {
    Int(i32),
    Name(String, Pos),
    /// `t.dx` (0) or `t.dy` (1).
    Field(String, u8, Pos),
    Sense(Sense),
    Bin(OpCode, Box<Expr>, Box<Expr>),
    Neg(Box<Expr>),
    Rand(Box<Expr>),
    Chance(Box<Expr>),
    Count(Pred, Box<Expr>),
    Dist(Target),
    FreeAt(Target),
    IsAt(Target, Pred),
    /// `look_of(t)`, `signal_of(t)`: the public bytes of whoever is there.
    LookOf(Target),
    SignalOf(Target, Pos),
    /// `scent(ch)` at the actor's cell, `scent(ch, t)` at a target.
    Scent(String, Option<Target>, Pos),
    /// `min`, `max`, `abs`, `sign`, `clamp`, `pack`, `hi`, `lo`: the opcode
    /// and its arguments.
    Fn(OpCode, Vec<Expr>),
    Call {
        name: String,
        args: Vec<Arg>,
        at: Pos,
    },
}

// ---- parser ---------------------------------------------------------------------------

struct Parser<'a> {
    file: &'a str,
    tokens: Vec<Token>,
    at: usize,
    /// How deep the tree being parsed is here, and the deepest level the
    /// current operand reached: see [`Self::nested`] and [`Self::chained`].
    depth: u32,
    deepest: u32,
}

/// How deep one tree may be: a nesting and an operator in a chain are one
/// level each (RULES §18). The parser, the code generator, lint and drop
/// all recurse on the tree, so this is what keeps them on the stack.
pub const MAX_DEPTH: u32 = 128;

// The other limits of a rule set (RULES §18; `tests/docs.rs` holds the doc
// to these).
/// The widest `sight`: a search stays inside the 3x3 chunk halo.
pub const MAX_SIGHT: u8 = 16;
/// The most `fuel` a kind may declare, in ops per think.
pub const MAX_FUEL: u32 = 4096;
/// `state` blocks per kind.
pub const MAX_STATES: usize = 64;
/// Tags per rule set: a kind's tags are one `u64` bitset.
pub const MAX_TAGS: usize = u64::BITS as usize;
/// Kinds per rule set: ids are `u16`, and `Kinds` keeps `u16::MAX` free.
pub const MAX_KINDS: usize = 65_534;
/// Subs per rule set, a member sub once per kind that has it: `Call` takes
/// a `u16`.
pub const MAX_SUBS: usize = 65_536;
/// Distinct constants outside the 16-bit immediates: the pool index is a
/// `u16`.
pub const MAX_POOL: usize = 65_536;
/// Rules files per rule set: a rule's and a kind's file in [`DebugInfo`]
/// is a `u16` index.
pub const MAX_FILES: usize = 65_536;
const _: () = assert!(
    MAX_SUBS == 1 << u16::BITS && MAX_POOL == 1 << u16::BITS && MAX_FILES == 1 << u16::BITS
);
const _: () = assert!(MAX_KINDS == u16::MAX as usize - 1);

fn sense_named(name: &str) -> Option<Sense> {
    Some(match name {
        "light" => Sense::Light,
        "age" => Sense::Age,
        "x" => Sense::X,
        "y" => Sense::Y,
        "hour" => Sense::Hour,
        "day" => Sense::Day,
        "kind" => Sense::Kind,
        "look" => Sense::Look,
        "signal" => Sense::Signal,
        "state" => Sense::State,
        "hurt" => Sense::Hurt,
        "hurt_dir" => Sense::HurtDir,
        "result" => Sense::Result,
        "taken" => Sense::Taken,
        "trapped" => Sense::Trapped,
        _ => return None,
    })
}

// `water`, `soil`, `rock` and `bare` are contextual: predicates after
// `count`/`nearest`/`is`/`random`, plain names elsewhere (so a kind may
// declare `need water`). `food` likewise: a declaration where a declaration
// starts, a need name everywhere else (`need food`, `food < 12h`). The
// built-in functions, `free` among them, are reserved.
pub const KEYWORDS: &[&str] = &[
    "kind",
    "sub",
    "glyph",
    "tags",
    "cadence",
    "sight",
    "fuel",
    "bite",
    "place",
    "need",
    "max",
    "decay",
    "vital",
    "mem",
    "when",
    "if",
    "else",
    "while",
    "repeat",
    "let",
    "return",
    "choose",
    "and",
    "or",
    "not",
    "nearest",
    "count",
    "within",
    "as",
    "true",
    "false",
    "idle",
    "die",
    "become",
    "spawn",
    "move",
    "drink",
    "eat",
    "hit",
    "at",
    "here",
    "toward",
    "away",
    "random",
    "attacker",
    "north",
    "east",
    "south",
    "west",
    "color",
    "dir",
    "blocked",
    "missed",
    "refused",
    "cover",
    "graze",
    "state",
    "next",
    "const",
    "for",
    "each",
    "look_of",
    "signal_of",
    "pack",
    "hi",
    "lo",
    "take",
    "give",
    "with",
    "mark",
    "sniff",
    "scent",
    "trait",
    "extends",
    "inherit",
    "only",
    "min",
    "abs",
    "sign",
    "clamp",
    "rand",
    "chance",
    "dist",
    "is",
    "free",
];
/// Words that start a declaration in a kind or trait body.
const DECL_WORDS: [&str; 12] = [
    "glyph", "tags", "cadence", "sight", "fuel", "food", "bite", "cover", "color", "place", "need",
    "mem",
];

const DIRS: [(&str, i32, i32); 4] = [
    ("north", 0, -1),
    ("east", 1, 0),
    ("south", 0, 1),
    ("west", -1, 0),
];

fn is_reserved(n: &str) -> bool {
    KEYWORDS.contains(&n) || sense_named(n).is_some()
}

impl Parser<'_> {
    fn peek(&self) -> &Tok {
        &self.tokens[self.at].tok
    }

    fn peek2(&self) -> &Tok {
        &self.tokens[(self.at + 1).min(self.tokens.len() - 1)].tok
    }

    fn pos(&self) -> Pos {
        let t = &self.tokens[self.at];
        Pos {
            file: self.file.to_string(),
            line: t.line,
            col: t.col,
        }
    }

    fn err_at(&self, at: &Pos, msg: impl Into<String>) -> CompileError {
        CompileError {
            file: at.file.clone(),
            line: at.line,
            col: at.col,
            msg: msg.into(),
        }
    }

    fn err(&self, msg: impl Into<String>) -> CompileError {
        self.err_at(&self.pos(), msg)
    }

    fn describe(&self) -> String {
        match self.peek() {
            Tok::Int(v) => format!("number {v}"),
            Tok::Time(_) => "time literal".into(),
            Tok::Str(s) => format!("string {s:?}"),
            Tok::Name(n) => format!("`{n}`"),
            Tok::Sym(s) => format!("`{s}`"),
            Tok::Eof => "end of file".into(),
        }
    }

    fn bump(&mut self) -> Tok {
        let t = self.tokens[self.at].tok.clone();
        if t != Tok::Eof {
            self.at += 1;
        }
        t
    }

    fn is_sym(&self, s: &str) -> bool {
        matches!(self.peek(), Tok::Sym(x) if *x == s)
    }

    fn is_kw(&self, k: &str) -> bool {
        matches!(self.peek(), Tok::Name(x) if x == k)
    }

    fn eat_sym(&mut self, s: &str) -> bool {
        if self.is_sym(s) {
            self.bump();
            true
        } else {
            false
        }
    }

    fn eat_kw(&mut self, k: &str) -> bool {
        if self.is_kw(k) {
            self.bump();
            true
        } else {
            false
        }
    }

    fn expect_sym(&mut self, s: &str) -> Result<()> {
        if self.eat_sym(s) {
            Ok(())
        } else {
            Err(self.err(format!("expected `{s}`, found {}", self.describe())))
        }
    }

    fn expect_kw(&mut self, k: &str) -> Result<()> {
        if self.eat_kw(k) {
            Ok(())
        } else {
            Err(self.err(format!("expected `{k}`, found {}", self.describe())))
        }
    }

    /// An identifier that is not a keyword or a sense.
    fn ident(&mut self, what: &str) -> Result<(String, Pos)> {
        let at = self.pos();
        match self.peek().clone() {
            Tok::Name(n) if !is_reserved(&n) => {
                self.bump();
                Ok((n, at))
            }
            Tok::Name(n) => Err(self.err(format!("`{n}` is a reserved word, not a {what}"))),
            _ => Err(self.err(format!("expected a {what}, found {}", self.describe()))),
        }
    }

    /// Parse with `f` one level deeper.
    fn nested<T>(&mut self, f: impl FnOnce(&mut Self) -> Result<T>) -> Result<T> {
        if self.depth == MAX_DEPTH {
            return Err(self.too_deep());
        }
        self.depth += 1;
        self.deepest = self.deepest.max(self.depth);
        let r = f(self);
        self.depth -= 1;
        r
    }

    /// One more operator in a chain: `l op r` is a level above the deeper of
    /// `l` (which reached `left`) and `r`, so `a + b + c` is two levels.
    fn chained(&mut self, left: u32) -> Result<()> {
        self.deepest = self.deepest.max(left) + 1;
        if self.deepest > MAX_DEPTH {
            return Err(self.too_deep());
        }
        Ok(())
    }

    /// Nesting and chains share one budget, so either one running out may
    /// be the other's doing: the error names both.
    fn too_deep(&self) -> CompileError {
        self.err(format!(
            "nested too deep (at most {MAX_DEPTH} levels; each operator in a chain counts as one)"
        ))
    }

    fn int(&mut self, what: &str) -> Result<i32> {
        match *self.peek() {
            Tok::Int(v) => {
                self.bump();
                Ok(v)
            }
            _ => Err(self.err(format!("expected {what}, found {}", self.describe()))),
        }
    }

    fn file(&mut self, items: &mut Items) -> Result<()> {
        while *self.peek() != Tok::Eof {
            if self.is_kw("kind") || self.is_kw("trait") {
                items.items.push(self.item()?);
            } else if self.is_kw("sub") {
                items.subs.push(self.sub()?);
            } else if self.eat_kw("const") {
                let (name, at) = self.ident("constant name")?;
                self.expect_sym("=")?;
                let value = self.expr()?;
                self.eat_sym(";");
                items.consts.push(ConstAst { at, name, value });
            } else {
                return Err(self.err(format!(
                    "expected `kind`, `trait`, `sub` or `const`, found {}",
                    self.describe()
                )));
            }
        }
        Ok(())
    }

    fn sub(&mut self) -> Result<SubAst> {
        self.expect_kw("sub")?;
        let (name, at) = self.ident("sub name")?;
        self.expect_sym("(")?;
        let mut params = Vec::new();
        if !self.is_sym(")") {
            loop {
                let (p, pat) = self.ident("parameter name")?;
                let ty = if self.eat_sym(":") {
                    match self.bump() {
                        Tok::Name(t) if t == "target" => Ty::Target,
                        Tok::Name(t) if t == "pred" => Ty::Pred,
                        _ => {
                            return Err(self.err_at(
                                &pat,
                                "a parameter type is `target` or `pred` (int needs none)",
                            ));
                        }
                    }
                } else {
                    Ty::Int
                };
                if params.iter().any(|(q, _)| *q == p) {
                    return Err(self.err_at(&pat, format!("parameter `{p}` declared twice")));
                }
                params.push((p, ty));
                if !self.eat_sym(",") {
                    break;
                }
            }
        }
        self.expect_sym(")")?;
        let body = self.block()?;
        let returns = returns_value(&body);
        Ok(SubAst {
            at,
            name,
            params,
            body,
            returns,
        })
    }

    fn item(&mut self) -> Result<ItemAst> {
        let is_trait = self.is_kw("trait");
        self.bump(); // `kind` or `trait`
        let (name, at) = self.ident(if is_trait { "trait name" } else { "kind name" })?;
        // `count water` is the ground: a kind of that name no pred could match.
        if !is_trait && pred_word(&name).is_some() {
            return Err(self.err_at(
                &at,
                format!("`{name}` is a predicate word, not a kind name"),
            ));
        }
        let mut params = Vec::new();
        if self.is_sym("(") {
            if !is_trait {
                return Err(self.err("a kind takes no parameters (only a trait does)"));
            }
            self.bump();
            if !self.is_sym(")") {
                loop {
                    let (p, pat) = self.ident("parameter name")?;
                    if params.iter().any(|(q, _)| *q == p) {
                        return Err(self.err_at(&pat, format!("parameter `{p}` declared twice")));
                    }
                    params.push((p, pat));
                    if !self.eat_sym(",") {
                        break;
                    }
                }
            }
            self.expect_sym(")")?;
        }
        let mut parents: Vec<ParentRef> = Vec::new();
        if self.eat_kw("extends") {
            loop {
                let (pname, pat) = self.ident("trait or kind name")?;
                let mut args = Vec::new();
                if self.eat_sym("(") {
                    if !self.is_sym(")") {
                        loop {
                            args.push(self.expr()?);
                            if !self.eat_sym(",") {
                                break;
                            }
                        }
                    }
                    self.expect_sym(")")?;
                }
                if parents.iter().any(|p| p.name == pname) {
                    return Err(self.err_at(&pat, format!("`{pname}` listed twice")));
                }
                parents.push(ParentRef {
                    name: pname,
                    args,
                    at: pat,
                });
                if !self.eat_sym(",") {
                    break;
                }
            }
        }
        self.expect_sym("{")?;
        // Declarations, member subs, reflex rules, states: in that order,
        // so a file reads top-down.
        let decls = self.decls(is_trait)?;
        let mut members: Vec<SubAst> = Vec::new();
        while self.is_kw("sub") {
            let sub = self.sub()?;
            if members.iter().any(|m| m.name == sub.name) {
                return Err(self.err_at(&sub.at, format!("sub `{}` declared twice", sub.name)));
            }
            members.push(sub);
        }
        let rules = self.rule_items()?;
        let mut states: Vec<StateAst> = Vec::new();
        while self.eat_kw("state") {
            let (sname, sat) = self.ident("state name")?;
            if states.iter().any(|s| s.name == sname) {
                return Err(self.err_at(&sat, format!("state `{sname}` declared twice")));
            }
            if states.len() == MAX_STATES {
                return Err(self.err_at(&sat, format!("at most {MAX_STATES} states per kind")));
            }
            self.expect_sym("{")?;
            let srules = self.rule_items()?;
            if !self.is_sym("}") {
                return Err(self.err(format!(
                    "expected `when`, `inherit` or `}}` in state `{sname}`, found {}",
                    self.describe()
                )));
            }
            self.expect_sym("}")?;
            states.push(StateAst {
                at: sat,
                name: sname,
                rules: srules,
            });
        }
        if !self.is_sym("}") {
            let what = if DECL_WORDS.iter().any(|w| self.is_kw(w)) {
                "declarations come first, before subs and rules".to_string()
            } else if self.is_kw("sub") {
                "member subs come before the rules".to_string()
            } else if !states.is_empty() && (self.is_kw("when") || self.is_kw("inherit")) {
                "reflex rules (`when`, `inherit`) come before the states".to_string()
            } else if !states.is_empty() {
                format!("expected `state` or `}}`, found {}", self.describe())
            } else {
                format!(
                    "expected `when`, `inherit`, `state` or `}}`, found {}",
                    self.describe()
                )
            };
            return Err(self.err(what));
        }
        self.expect_sym("}")?;
        Ok(ItemAst {
            at,
            name,
            is_trait,
            params,
            parents,
            decls,
            members,
            rules,
            states,
        })
    }

    /// The declarations at the top of a kind or trait body.
    fn decls(&mut self, is_trait: bool) -> Result<Decls> {
        let mut d = Decls::default();
        loop {
            let p = self.pos();
            let not_in_trait = |what: &str| format!("a trait has no {what}: only a kind does");
            // A single-valued declaration is given once per body.
            let given = [
                ("glyph", d.glyph.is_some()),
                ("color", d.color.is_some()),
                ("cover", d.cover),
                ("cadence", d.cadence.is_some()),
                ("sight", d.sight.is_some()),
                ("fuel", d.fuel.is_some()),
                ("food", d.food.is_some()),
                ("bite", d.bite.is_some()),
            ];
            if let Some((w, _)) = given.iter().find(|(w, set)| *set && self.is_kw(w)) {
                return Err(self.err_at(&p, format!("`{w}` declared twice")));
            }
            if self.eat_kw("glyph") {
                if is_trait {
                    return Err(self.err_at(&p, not_in_trait("glyph")));
                }
                match self.bump() {
                    Tok::Str(s) if s.len() == 1 && s.as_bytes()[0].is_ascii_graphic() => {
                        d.glyph = Some(s.as_bytes()[0]);
                    }
                    _ => {
                        return Err(self.err_at(&p, "glyph takes one printable ASCII character"));
                    }
                }
            } else if self.eat_kw("tags") {
                // The list ends where the next thing in the body starts
                // (`food` is not reserved but starts a declaration).
                while let Tok::Name(n) = self.peek().clone() {
                    if DECL_WORDS.contains(&n.as_str())
                        || ["when", "inherit", "state", "sub"].contains(&n.as_str())
                    {
                        break;
                    }
                    if is_reserved(&n) {
                        return Err(self.err(format!("`{n}` is a reserved word, not a tag")));
                    }
                    if pred_word(&n).is_some() {
                        return Err(self.err(format!("`{n}` is a predicate word, not a tag")));
                    }
                    self.bump();
                    d.tags.push(n);
                }
            } else if self.eat_kw("cadence") {
                d.cadence = Some((self.additive()?, p));
            } else if self.eat_kw("sight") {
                d.sight = Some((self.additive()?, p));
            } else if self.eat_kw("fuel") {
                d.fuel = Some((self.additive()?, p));
            } else if self.eat_kw("food") {
                d.food = Some((self.additive()?, p));
            } else if self.eat_kw("bite") {
                d.bite = Some((self.additive()?, p));
            } else if self.eat_kw("cover") {
                d.cover = true;
            } else if self.eat_kw("color") {
                if is_trait {
                    return Err(self.err_at(&p, not_in_trait("colour")));
                }
                // `color "#rrggbb"`
                let c = match self.bump() {
                    Tok::Str(s)
                        if s.len() == 7
                            && s.starts_with('#')
                            && s[1..].bytes().all(|b| b.is_ascii_hexdigit()) =>
                    {
                        u32::from_str_radix(&s[1..], 16).ok()
                    }
                    _ => None,
                };
                d.color = Some(c.ok_or_else(|| self.err_at(&p, "color takes \"#rrggbb\""))?);
            } else if self.eat_kw("place") {
                // Where kinds start is the world's business, not the rules'.
                return Err(self.err_at(
                    &p,
                    "`place` moved to the scenario: write `start KIND N / D` in a .scenario file",
                ));
            } else if self.eat_kw("need") {
                let (name, ..) = self.ident("need name")?;
                self.expect_kw("max")?;
                let max = self.additive()?;
                let mut decays = true;
                if self.eat_kw("decay") {
                    match self.int("0 (points) or 1 (per tick)")? {
                        0 => decays = false,
                        1 => decays = true,
                        _ => return Err(self.err_at(&p, "decay is 0 (points) or 1 (per tick)")),
                    }
                }
                let vital = self.eat_kw("vital");
                if d.needs.iter().any(|n| n.name == name) {
                    return Err(self.err_at(&p, format!("need `{name}` declared twice")));
                }
                d.needs.push(NeedAst {
                    name,
                    max,
                    decays,
                    vital,
                    at: p,
                });
            } else if self.eat_kw("mem") {
                loop {
                    let (name, at) = self.ident("memory slot name")?;
                    if d.mems.iter().any(|(m, _)| *m == name)
                        || d.needs.iter().any(|n| n.name == name)
                    {
                        return Err(self.err_at(&at, format!("`{name}` declared twice")));
                    }
                    d.mems.push((name, at));
                    if !self.eat_sym(",") {
                        break;
                    }
                }
            } else {
                return Ok(d);
            }
            self.eat_sym(";");
        }
    }

    /// `when cond => body` and `inherit [NAME]`, as many as there are.
    fn rule_items(&mut self) -> Result<Vec<RuleItem>> {
        let mut rules = Vec::new();
        loop {
            let at = self.pos();
            if self.eat_kw("inherit") {
                let name = match self.peek().clone() {
                    Tok::Name(n) if !is_reserved(&n) => {
                        self.bump();
                        Some(n)
                    }
                    _ => None,
                };
                rules.push(RuleItem::Inherit(name, at));
                continue;
            }
            if !self.eat_kw("when") {
                break;
            }
            let cond = self.cond()?;
            let arrow_line = self.pos().line;
            self.expect_sym("=>")?;
            // One statement may end in `;`, as in a block; a block may not.
            let single = !self.is_sym("{");
            let body = self.body()?;
            if single {
                self.eat_sym(";");
            }
            rules.push(RuleItem::When(Box::new(Rule {
                at,
                arrow_line,
                cond,
                body,
            })));
        }
        Ok(rules)
    }

    fn body(&mut self) -> Result<Vec<Stmt>> {
        if self.is_sym("{") {
            self.block()
        } else {
            Ok(vec![self.nested(Self::stmt)?])
        }
    }

    fn block(&mut self) -> Result<Vec<Stmt>> {
        self.expect_sym("{")?;
        let mut stmts = Vec::new();
        while !self.is_sym("}") {
            if *self.peek() == Tok::Eof {
                return Err(self.err("unclosed block"));
            }
            stmts.push(self.nested(Self::stmt)?);
            self.eat_sym(";");
        }
        self.expect_sym("}")?;
        Ok(stmts)
    }

    fn stmt(&mut self) -> Result<Stmt> {
        let at = self.pos();
        if self.eat_kw("if") {
            let cond = self.cond()?;
            let then = self.block()?;
            let els = if self.eat_kw("else") {
                if self.is_kw("if") {
                    vec![self.nested(Self::stmt)?]
                } else {
                    self.block()?
                }
            } else {
                Vec::new()
            };
            return Ok(Stmt::If { cond, then, els });
        }
        if self.eat_kw("while") {
            let cond = self.cond()?;
            let body = self.block()?;
            return Ok(Stmt::While { cond, body });
        }
        if self.eat_kw("repeat") {
            let count = self.expr()?;
            let body = self.block()?;
            return Ok(Stmt::Repeat { count, body });
        }
        if self.eat_kw("let") {
            let (name, at) = self.ident("local name")?;
            self.expect_sym("=")?;
            let value = self.expr()?;
            return Ok(Stmt::Let { name, at, value });
        }
        if self.eat_kw("return") {
            // The value starts on the `return` line, or there is none.
            let value = if self.is_sym("}") || self.is_sym(";") || self.pos().line != at.line {
                None
            } else {
                Some(self.expr()?)
            };
            return Ok(Stmt::Return { value, at });
        }
        if self.eat_kw("choose") {
            self.expect_sym("{")?;
            let mut arms = Vec::new();
            while !self.is_sym("}") {
                if *self.peek() == Tok::Eof {
                    return Err(self.err_at(&at, "unclosed `choose`"));
                }
                let w = self.expr()?;
                self.expect_sym(":")?;
                let body = self.body()?;
                arms.push((w, body));
                self.eat_sym(";");
            }
            self.expect_sym("}")?;
            if arms.is_empty() {
                return Err(self.err_at(&at, "choose needs at least one arm"));
            }
            return Ok(Stmt::Choose(arms));
        }
        if self.eat_kw("idle") {
            return Ok(Stmt::Idle(at));
        }
        if self.eat_kw("die") {
            return Ok(Stmt::Die(at));
        }
        if self.eat_kw("become") {
            let (kind, at) = self.ident("kind name")?;
            return Ok(Stmt::Become { kind, at });
        }
        if self.eat_kw("spawn") {
            let (kind, pos) = self.ident("kind name")?;
            self.expect_kw("at")?;
            let at = self.target()?;
            let mut with: Vec<(String, Expr, Pos)> = Vec::new();
            if self.eat_kw("with") {
                self.expect_sym("(")?;
                loop {
                    let named = matches!(self.peek(), Tok::Name(_))
                        && matches!(self.peek2(), Tok::Sym("="));
                    if !named {
                        return Err(self.err(
                            "`with` names the memory it sets: `with (home_x = x, home_y = y)`",
                        ));
                    }
                    let (name, npos) = self.ident("memory name")?;
                    self.expect_sym("=")?;
                    let value = self.expr()?;
                    if with.iter().any(|(n, ..)| *n == name) {
                        return Err(self.err_at(&npos, format!("`with` sets `{name}` twice")));
                    }
                    with.push((name, value, npos));
                    if !self.eat_sym(",") {
                        break;
                    }
                }
                self.expect_sym(")")?;
                if with.len() > 2 {
                    return Err(self.err_at(&with[2].2, "`with` sets at most two memory slots"));
                }
            }
            return Ok(Stmt::Spawn {
                kind,
                at,
                pos,
                with,
            });
        }
        if self.is_kw("take") || self.is_kw("give") {
            let give = self.is_kw("give");
            self.bump();
            let target = self.target()?;
            let (need, _) = self.ident("need name")?;
            let amount = self.expr()?;
            return Ok(Stmt::Transfer {
                give,
                target,
                need,
                amount,
                at,
            });
        }
        if self.eat_kw("move") {
            return Ok(Stmt::Move(self.target()?, at));
        }
        if self.eat_kw("drink") {
            return Ok(Stmt::Drink(self.target()?, at));
        }
        if self.eat_kw("eat") {
            return Ok(Stmt::Eat(self.target()?, at));
        }
        if self.eat_kw("hit") {
            return Ok(Stmt::Hit(self.target()?, at));
        }
        if self.eat_kw("graze") {
            return Ok(Stmt::Graze(self.target()?, at));
        }
        if self.is_kw("look") && matches!(self.peek2(), Tok::Sym("=")) {
            self.bump();
            self.bump();
            return Ok(Stmt::Look(self.expr()?));
        }
        if self.is_kw("signal") && matches!(self.peek2(), Tok::Sym("=")) {
            self.bump();
            self.bump();
            return Ok(Stmt::Signal(self.expr()?));
        }
        if self.eat_kw("next") {
            let (name, at) = self.ident("state name")?;
            return Ok(Stmt::Next(name, at));
        }
        if self.eat_kw("mark") {
            let (ch, at) = self.ident("scent name")?;
            return Ok(Stmt::Mark(ch, self.expr()?, at));
        }
        if self.eat_kw("for") {
            self.expect_kw("each")?;
            let pred = self.pred()?;
            self.expect_kw("within")?;
            let r = self.additive()?; // a radius, never a comparison
            self.expect_kw("as")?;
            let (bind, ..) = self.ident("binding name")?;
            let body = self.block()?;
            return Ok(Stmt::ForEach {
                pred,
                r,
                bind,
                body,
                at,
            });
        }
        // A call or an assignment.
        let (name, at) = self.ident("statement")?;
        if self.is_sym("(") {
            let args = self.args()?;
            return Ok(Stmt::Call { name, args, at });
        }
        if self.eat_sym("=") {
            let value = self.expr()?;
            return Ok(Stmt::Set { name, at, value });
        }
        for (sym, op) in [("+=", OpCode::Add), ("-=", OpCode::Sub)] {
            if self.eat_sym(sym) {
                let value = self.expr()?;
                return Ok(Stmt::Assign {
                    name,
                    at,
                    op,
                    value,
                });
            }
        }
        Err(self.err(format!(
            "expected `(`, `=`, `+=` or `-=` after `{name}`, found {}",
            self.describe()
        )))
    }

    /// `( arg, ... )`. A bare name is typed by the callee's parameter.
    fn args(&mut self) -> Result<Vec<Arg>> {
        self.expect_sym("(")?;
        let mut args = Vec::new();
        if !self.is_sym(")") {
            loop {
                let at = self.pos();
                let arg = if self.starts_target() {
                    Arg::Target(self.target()?)
                } else if self.is_kw("only")
                    || (matches!(self.peek(), Tok::Name(n) if !is_reserved(n))
                        && matches!(self.peek2(), Tok::Sym(":")))
                {
                    Arg::Pred(self.pred()?)
                } else if self.is_kw("free")
                    && matches!(self.peek2(), Tok::Sym(",") | Tok::Sym(")"))
                {
                    self.bump();
                    Arg::Pred(Pred::Free)
                } else if let Tok::Name(n) = self.peek().clone()
                    && !is_reserved(&n)
                    && matches!(self.peek2(), Tok::Sym(",") | Tok::Sym(")"))
                {
                    self.bump();
                    Arg::Name(n, at)
                } else {
                    Arg::Expr(self.expr()?)
                };
                args.push(arg);
                if !self.eat_sym(",") {
                    break;
                }
            }
        }
        self.expect_sym(")")?;
        Ok(args)
    }

    fn starts_target(&self) -> bool {
        matches!(self.peek(), Tok::Name(n) if ["here", "attacker", "toward", "away", "random", "north", "east", "south", "west"].contains(&n.as_str()))
            || ((self.is_kw("at") || self.is_kw("dir")) && matches!(self.peek2(), Tok::Sym("(")))
    }

    fn target(&mut self) -> Result<Target> {
        let at = self.pos();
        if self.eat_kw("here") {
            return Ok(Target::Here);
        }
        if self.eat_kw("attacker") {
            return Ok(Target::Attacker);
        }
        if self.eat_kw("dir") {
            self.expect_sym("(")?;
            let h = self.expr()?;
            self.expect_sym(")")?;
            return Ok(Target::Heading(Box::new(h)));
        }
        if self.eat_kw("toward") {
            return Ok(Target::Toward(Box::new(self.nested(Self::target)?)));
        }
        if self.eat_kw("away") {
            return Ok(Target::Away(Box::new(self.nested(Self::target)?)));
        }
        if self.eat_kw("random") {
            self.expect_kw("free")?;
            return Ok(Target::RandomFree);
        }
        if self.eat_kw("at") {
            self.expect_sym("(")?;
            let x = self.expr()?;
            self.expect_sym(",")?;
            let y = self.expr()?;
            self.expect_sym(")")?;
            return Ok(Target::At(Box::new(x), Box::new(y)));
        }
        for (name, dx, dy) in DIRS {
            if self.eat_kw(name) {
                return Ok(Target::Dir(dx, dy));
            }
        }
        let (name, _) = self.ident(
            "target (a binding, here, attacker, dir(h), north/east/south/west, toward, away, at(x, y) or random free)",
        )?;
        Ok(Target::Named(name, at))
    }

    // cond := and_cond ("or" and_cond)*
    fn cond(&mut self) -> Result<Cond> {
        let outer = std::mem::replace(&mut self.deepest, self.depth);
        let mut c = self.and_cond()?;
        while self.eat_kw("or") {
            let left = std::mem::replace(&mut self.deepest, self.depth);
            let r = self.and_cond()?;
            self.chained(left)?;
            c = Cond::Or(Box::new(c), Box::new(r));
        }
        self.deepest = self.deepest.max(outer);
        Ok(c)
    }

    fn and_cond(&mut self) -> Result<Cond> {
        let outer = std::mem::replace(&mut self.deepest, self.depth);
        let mut c = self.not_cond()?;
        while self.eat_kw("and") {
            let left = std::mem::replace(&mut self.deepest, self.depth);
            let r = self.not_cond()?;
            self.chained(left)?;
            c = Cond::And(Box::new(c), Box::new(r));
        }
        self.deepest = self.deepest.max(outer);
        Ok(c)
    }

    fn not_cond(&mut self) -> Result<Cond> {
        if self.eat_kw("not") {
            return Ok(Cond::Not(Box::new(self.nested(Self::not_cond)?)));
        }
        let at = self.pos();
        if self.eat_kw("nearest") {
            let pred = self.pred()?;
            self.expect_kw("within")?;
            let r = self.additive()?; // a radius, never a comparison
            self.expect_kw("as")?;
            let (bind, ..) = self.ident("binding name")?;
            return Ok(Cond::Nearest { pred, r, bind, at });
        }
        if self.eat_kw("sniff") {
            let (ch, _) = self.ident("scent name")?;
            self.expect_kw("within")?;
            let r = self.additive()?; // a radius, never a comparison
            self.expect_kw("as")?;
            let (bind, ..) = self.ident("binding name")?;
            return Ok(Cond::Sniff { ch, r, bind, at });
        }
        if self.is_sym("(") {
            // Either a parenthesised condition or a parenthesised expression
            // starting a comparison; parse as a cond and let expr-level
            // parentheses handle the rest. If both fail, the error that got
            // further into the tokens is the real one.
            let save = (self.at, self.deepest);
            self.bump();
            let mut cond_err = None;
            match self.nested(Self::cond) {
                Ok(c) => match self.expect_sym(")") {
                    Ok(()) if !self.starts_binop() => return Ok(c),
                    Ok(()) => {}
                    Err(e) => cond_err = Some((self.at, e)),
                },
                Err(e) => cond_err = Some((self.at, e)),
            }
            (self.at, self.deepest) = save;
            return match self.expr() {
                Ok(e) => Ok(Cond::Expr(e)),
                Err(e) => match cond_err {
                    Some((at, ce)) if at > self.at => Err(ce),
                    _ => Err(e),
                },
            };
        }
        Ok(Cond::Expr(self.expr()?))
    }

    fn starts_binop(&self) -> bool {
        matches!(self.peek(), Tok::Sym(s) if ["+", "-", "*", "/", "%", "<", "<=", "==", "!=", ">=", ">"].contains(s))
    }

    fn pred(&mut self) -> Result<Pred> {
        let at = self.pos();
        let only = self.eat_kw("only");
        if let Tok::Name(word) = self.peek().clone()
            && let Some(p) = pred_word(&word)
        {
            self.bump();
            if only {
                return Err(self.err_at(&at, format!("`only` applies to a kind, not `{word}`")));
            }
            return Ok(p);
        }
        let (name, ..) =
            self.ident("predicate (a kind, a tag, water, soil, rock, free or bare)")?;
        if self.eat_sym(":") {
            let look = self.int("a look value (0 to 255)")?;
            let look =
                u8::try_from(look).map_err(|_| self.err_at(&at, "a look value is 0 to 255"))?;
            return Ok(Pred::KindLook(name, look, only, at));
        }
        Ok(Pred::Kind(name, only, at))
    }

    fn cmp_op(&self) -> Option<OpCode> {
        match self.peek() {
            Tok::Sym("<") => Some(OpCode::Lt),
            Tok::Sym("<=") => Some(OpCode::Le),
            Tok::Sym("==") => Some(OpCode::Eq),
            Tok::Sym("!=") => Some(OpCode::Ne),
            Tok::Sym(">=") => Some(OpCode::Ge),
            Tok::Sym(">") => Some(OpCode::Gt),
            _ => None,
        }
    }

    // expr := additive [cmp additive]: comparisons don't chain.
    fn expr(&mut self) -> Result<Expr> {
        let outer = std::mem::replace(&mut self.deepest, self.depth);
        let mut e = self.additive()?;
        if let Some(op) = self.cmp_op() {
            self.bump();
            let left = std::mem::replace(&mut self.deepest, self.depth);
            let r = self.additive()?;
            self.chained(left)?;
            e = Expr::Bin(op, Box::new(e), Box::new(r));
            if self.cmp_op().is_some() {
                return Err(self.err(
                    "comparisons don't chain: join two with `and` (`a < b and b < c`), \
                     or parenthesise the first (`(a < b) < c` compares 0 or 1 with c)",
                ));
            }
        }
        self.deepest = self.deepest.max(outer);
        Ok(e)
    }

    fn additive(&mut self) -> Result<Expr> {
        let outer = std::mem::replace(&mut self.deepest, self.depth);
        let mut e = self.term()?;
        loop {
            let op = match self.peek() {
                Tok::Sym("+") => OpCode::Add,
                Tok::Sym("-") => OpCode::Sub,
                _ => break,
            };
            self.bump();
            let left = std::mem::replace(&mut self.deepest, self.depth);
            let r = self.term()?;
            self.chained(left)?;
            e = Expr::Bin(op, Box::new(e), Box::new(r));
        }
        self.deepest = self.deepest.max(outer);
        Ok(e)
    }

    fn term(&mut self) -> Result<Expr> {
        let outer = std::mem::replace(&mut self.deepest, self.depth);
        let mut e = self.unary()?;
        loop {
            let op = match self.peek() {
                Tok::Sym("*") => OpCode::Mul,
                Tok::Sym("/") => OpCode::Div,
                Tok::Sym("%") => OpCode::Mod,
                _ => break,
            };
            self.bump();
            let left = std::mem::replace(&mut self.deepest, self.depth);
            let r = self.unary()?;
            self.chained(left)?;
            e = Expr::Bin(op, Box::new(e), Box::new(r));
        }
        self.deepest = self.deepest.max(outer);
        Ok(e)
    }

    fn unary(&mut self) -> Result<Expr> {
        if self.eat_sym("-") {
            return Ok(match self.nested(Self::unary)? {
                Expr::Int(v) => Expr::Int(v.wrapping_neg()),
                e => Expr::Neg(Box::new(e)),
            });
        }
        self.nested(Self::primary)
    }

    fn primary(&mut self) -> Result<Expr> {
        let at = self.pos();
        match self.bump() {
            Tok::Int(v) | Tok::Time(v) => Ok(Expr::Int(v)),
            Tok::Sym("(") => {
                let e = self.expr()?;
                self.expect_sym(")")?;
                Ok(e)
            }
            Tok::Name(n) => {
                if let Some(s) = sense_named(&n) {
                    return Ok(Expr::Sense(s));
                }
                match n.as_str() {
                    "true" => Ok(Expr::Int(1)),
                    "false" => Ok(Expr::Int(0)),
                    // The last action's result, as conditions.
                    "blocked" | "missed" | "refused" => {
                        let code = match n.as_str() {
                            "blocked" => result::BLOCKED,
                            "missed" => result::MISSED,
                            _ => result::REFUSED,
                        };
                        Ok(Expr::Bin(
                            OpCode::Eq,
                            Box::new(Expr::Sense(Sense::Result)),
                            Box::new(Expr::Int(i32::from(code))),
                        ))
                    }
                    "count" => {
                        let pred = self.pred()?;
                        self.expect_kw("within")?;
                        // One term: an operator after it applies to the count.
                        let r = self.unary()?;
                        Ok(Expr::Count(pred, Box::new(r)))
                    }
                    "rand" | "chance" => {
                        self.expect_sym("(")?;
                        let e = self.expr()?;
                        self.expect_sym(")")?;
                        Ok(if n == "rand" {
                            Expr::Rand(Box::new(e))
                        } else {
                            Expr::Chance(Box::new(e))
                        })
                    }
                    "dist" => {
                        self.expect_sym("(")?;
                        let t = self.target()?;
                        self.expect_sym(")")?;
                        Ok(Expr::Dist(t))
                    }
                    "free" => {
                        self.expect_sym("(")?;
                        let t = self.target()?;
                        self.expect_sym(")")?;
                        Ok(Expr::FreeAt(t))
                    }
                    "is" => {
                        self.expect_sym("(")?;
                        let t = self.target()?;
                        self.expect_sym(",")?;
                        let p = self.pred()?;
                        self.expect_sym(")")?;
                        Ok(Expr::IsAt(t, p))
                    }
                    "scent" => {
                        self.expect_sym("(")?;
                        let (ch, _) = self.ident("scent name")?;
                        let t = if self.eat_sym(",") {
                            Some(self.target()?)
                        } else {
                            None
                        };
                        self.expect_sym(")")?;
                        Ok(Expr::Scent(ch, t, at))
                    }
                    "look_of" | "signal_of" => {
                        self.expect_sym("(")?;
                        let t = self.target()?;
                        self.expect_sym(")")?;
                        Ok(if n == "look_of" {
                            Expr::LookOf(t)
                        } else {
                            Expr::SignalOf(t, at)
                        })
                    }
                    "min" | "max" | "abs" | "sign" | "clamp" | "pack" | "hi" | "lo" => {
                        let (op, arity) = match n.as_str() {
                            "min" => (OpCode::Min, 2),
                            "max" => (OpCode::Max, 2),
                            "abs" => (OpCode::Abs, 1),
                            "sign" => (OpCode::Sign, 1),
                            "pack" => (OpCode::Pack, 2),
                            "hi" => (OpCode::Hi, 1),
                            "lo" => (OpCode::Lo, 1),
                            _ => (OpCode::Clamp, 3),
                        };
                        self.expect_sym("(")?;
                        let mut args = vec![self.expr()?];
                        while self.eat_sym(",") {
                            args.push(self.expr()?);
                        }
                        self.expect_sym(")")?;
                        if args.len() != arity {
                            return Err(self.err_at(&at, format!("`{n}` takes {arity} arguments")));
                        }
                        Ok(Expr::Fn(op, args))
                    }
                    _ if is_reserved(&n) => {
                        Err(self.err_at(&at, format!("unexpected `{n}` in an expression")))
                    }
                    _ => {
                        if self.is_sym("(") {
                            let args = self.args()?;
                            return Ok(Expr::Call { name: n, args, at });
                        }
                        if self.eat_sym(".") {
                            let field = match self.bump() {
                                Tok::Name(f) if f == "dx" => 0,
                                Tok::Name(f) if f == "dy" => 1,
                                _ => {
                                    return Err(
                                        self.err_at(&at, format!("`{n}.` must be `.dx` or `.dy`"))
                                    );
                                }
                            };
                            return Ok(Expr::Field(n, field, at));
                        }
                        Ok(Expr::Name(n, at))
                    }
                }
            }
            t => {
                // `bump` stays put at the end of the file.
                if t != Tok::Eof {
                    self.at -= 1;
                }
                Err(self.err(format!("expected an expression, found {}", self.describe())))
            }
        }
    }
}

/// Does any `return` in this body carry a value?
fn returns_value(body: &[Stmt]) -> bool {
    body.iter().any(|s| match s {
        Stmt::Return { value, .. } => value.is_some(),
        Stmt::If { then, els, .. } => returns_value(then) || returns_value(els),
        Stmt::While { body, .. } | Stmt::Repeat { body, .. } | Stmt::ForEach { body, .. } => {
            returns_value(body)
        }
        Stmt::Choose(arms) => arms.iter().any(|(_, b)| returns_value(b)),
        Stmt::Set { .. }
        | Stmt::Assign { .. }
        | Stmt::Let { .. }
        | Stmt::Call { .. }
        | Stmt::Idle(_)
        | Stmt::Die(_)
        | Stmt::Become { .. }
        | Stmt::Spawn { .. }
        | Stmt::Transfer { .. }
        | Stmt::Move(..)
        | Stmt::Drink(..)
        | Stmt::Eat(..)
        | Stmt::Hit(..)
        | Stmt::Graze(..)
        | Stmt::Look(_)
        | Stmt::Signal(_)
        | Stmt::Mark(..)
        | Stmt::Next(..) => false,
    })
}

/// Every `spawn K ... with (...)` in `body`: the kind, and the names set.
fn spawn_withs<'b>(body: &'b [Stmt], out: &mut Vec<(&'b str, &'b [(String, Expr, Pos)])>) {
    for s in body {
        match s {
            Stmt::Spawn { kind, with, .. } if !with.is_empty() => out.push((kind, with)),
            Stmt::If { then, els, .. } => {
                spawn_withs(then, out);
                spawn_withs(els, out);
            }
            Stmt::While { body, .. } | Stmt::Repeat { body, .. } | Stmt::ForEach { body, .. } => {
                spawn_withs(body, out)
            }
            Stmt::Choose(arms) => arms.iter().for_each(|(_, b)| spawn_withs(b, out)),
            Stmt::Spawn { .. }
            | Stmt::Set { .. }
            | Stmt::Assign { .. }
            | Stmt::Let { .. }
            | Stmt::Call { .. }
            | Stmt::Return { .. }
            | Stmt::Idle(_)
            | Stmt::Die(_)
            | Stmt::Become { .. }
            | Stmt::Transfer { .. }
            | Stmt::Move(..)
            | Stmt::Drink(..)
            | Stmt::Eat(..)
            | Stmt::Hit(..)
            | Stmt::Graze(..)
            | Stmt::Look(_)
            | Stmt::Signal(_)
            | Stmt::Mark(..)
            | Stmt::Next(..) => {}
        }
    }
}

// ---- code generation ------------------------------------------------------------------

#[derive(Debug, Clone)]
struct Local {
    name: String,
    slot: u8,
    ty: Ty,
}

/// One rule of a resolved list: the rule, and the instance that wrote it
/// (whose parameters are in scope and whose tables bound what it may name).
#[derive(Debug, Clone, Copy)]
struct RRule<'a> {
    owner: usize,
    rule: &'a Rule,
}

/// A rule list after inheritance: the rules in the order a think scans
/// them, and every instance whose own rules are in it (so a splice never
/// brings the same rules twice).
#[derive(Debug, Clone, Default)]
struct RList<'a> {
    rules: Vec<RRule<'a>>,
    sources: Vec<usize>,
}

/// A trait or kind with its arguments bound and everything it inherits
/// merged: what codegen compiles (a concrete kind) or checks (a trait).
#[derive(Debug, Clone)]
struct Inst<'a> {
    item: usize,
    args: Vec<i32>,
    /// Direct parents, and every ancestor in linearized order.
    parents: Vec<usize>,
    ancestors: Vec<usize>,
    /// The longest chain of `extends` below it: 0 for a root.
    depth: u32,
    glyph: Option<u8>,
    color: Option<u32>,
    cover: bool,
    cadence_shift: Option<u8>,
    sight: Option<u8>,
    fuel: Option<u32>,
    food: Option<i32>,
    bite: Option<u8>,
    tags: Vec<String>,
    needs: Vec<NeedDef>,
    mems: Vec<String>,
    states: Vec<String>,
    /// Member subs after overriding: name, the instance that defines it,
    /// the sub.
    members: Vec<(String, usize, &'a SubAst)>,
    reflex: RList<'a>,
    /// Parallel to `states`.
    state_lists: Vec<RList<'a>>,
}

impl<'a> Inst<'a> {
    fn state_list(&self, name: &str) -> Option<&RList<'a>> {
        self.states
            .iter()
            .position(|s| s == name)
            .map(|i| &self.state_lists[i])
    }
}

struct Gen<'a> {
    items: &'a [ItemAst],
    subs: &'a [SubAst],
    consts: &'a [ConstAst],
    /// Folded `const` values, in declaration order.
    const_vals: Vec<(String, i32)>,
    asm: Asm,
    pool: Vec<i32>,
    /// Every resolved instance: each kind, each trait per argument list.
    insts: Vec<Inst<'a>>,
    /// Kind id -> its instance; item -> kind id (concrete kinds only).
    kind_insts: Vec<usize>,
    item_ids: Vec<Option<u16>>,
    /// Per kind id: its member subs' indices in the sub table.
    member_index: Vec<Vec<(String, u16)>>,
    /// The kind whose code is being compiled (for [`RuleInfo`]).
    kind: Option<u16>,
    /// The instance whose tables (needs, mems, states, member subs) the
    /// code uses: the kind being compiled, or the trait being checked.
    /// `None` in a file sub, which sees only its parameters.
    cur: Option<usize>,
    /// The instance that wrote the rule or member sub being compiled: its
    /// parameters are in scope, and it may name only what it declares.
    owner: Option<usize>,
    /// Member subs callable here: name -> sub table index.
    members_here: Vec<(String, u16)>,
    /// The sub being compiled, if any.
    sub: Option<&'a SubAst>,
    /// Trait parameters in scope, bound: constants.
    params: Vec<(String, i32)>,
    /// Checking traits on their own: declaration ranges are lenient and
    /// nothing compiled is kept.
    checking: bool,
    /// The instance being resolved has arguments made from the trait
    /// check's placeholder 1s: a trait it reaches twice is not reported.
    placeholder: bool,
    /// Instances that skipped such a conflict, or extend one that did: never
    /// reused for one whose arguments are real.
    lenient: Vec<usize>,
    locals: Vec<Local>,
    next_local: u8,
    /// Position for errors without a better one.
    here: Pos,
    /// Tag names, in first-appearance order (item order, then declaration
    /// order): a tag's bit is its index here.
    tags: Vec<String>,
    /// Scent channel names, in first-appearance order in the code.
    scents: Vec<String>,
    /// Per kind named by a `spawn ... with`: the memory slots those spawns
    /// set, in first-appearance order (at most two), each with where it
    /// was first named. They take the kind's first slots, which is where
    /// a spawn's two values land.
    withs: Vec<(String, Vec<(String, Pos)>)>,
    /// The source texts, for the rule table in [`DebugInfo`].
    files: &'a [(&'a str, &'a str)],
    debug: DebugInfo,
    /// The state whose rules are being compiled (`None`: the reflexes).
    state: Option<u8>,
    /// [`Gen::ends`] of a call, by (sub address, tables, depth, acts_only):
    /// only looked up, never iterated.
    ends_memo: RefCell<HashMap<(usize, Option<usize>, u32, bool), bool>>,
    /// [`Gen::call_acts`] of a call, by (sub address, tables, depth).
    acts_memo: RefCell<HashMap<(usize, Option<usize>, u32), bool>>,
    /// Compiling a `when` condition: a sub called here must not act.
    in_when: bool,
}

/// Does this statement emit an action? Its position, if so.
fn action_at(s: &Stmt) -> Option<&Pos> {
    match s {
        Stmt::Idle(at)
        | Stmt::Die(at)
        | Stmt::Become { at, .. }
        | Stmt::Spawn { pos: at, .. }
        | Stmt::Transfer { at, .. }
        | Stmt::Move(_, at)
        | Stmt::Drink(_, at)
        | Stmt::Eat(_, at)
        | Stmt::Hit(_, at)
        | Stmt::Graze(_, at) => Some(at),
        Stmt::Set { .. }
        | Stmt::Assign { .. }
        | Stmt::Let { .. }
        | Stmt::If { .. }
        | Stmt::While { .. }
        | Stmt::Repeat { .. }
        | Stmt::Choose(_)
        | Stmt::Call { .. }
        | Stmt::Return { .. }
        | Stmt::Look(_)
        | Stmt::Signal(_)
        | Stmt::Mark(..)
        | Stmt::Next(..)
        | Stmt::ForEach { .. } => None,
    }
}

/// Is this statement a `return`, or does it hold one? (Not through a call:
/// a callee's `return` leaves only the callee.)
fn may_return(s: &Stmt) -> bool {
    match s {
        Stmt::Return { .. } => true,
        Stmt::If { then, els, .. } => then.iter().chain(els).any(may_return),
        Stmt::Choose(arms) => arms.iter().any(|(_, body)| body.iter().any(may_return)),
        Stmt::While { body, .. } | Stmt::Repeat { body, .. } | Stmt::ForEach { body, .. } => {
            body.iter().any(may_return)
        }
        Stmt::Set { .. }
        | Stmt::Assign { .. }
        | Stmt::Let { .. }
        | Stmt::Call { .. }
        | Stmt::Idle(_)
        | Stmt::Die(_)
        | Stmt::Become { .. }
        | Stmt::Spawn { .. }
        | Stmt::Transfer { .. }
        | Stmt::Move(..)
        | Stmt::Drink(..)
        | Stmt::Eat(..)
        | Stmt::Hit(..)
        | Stmt::Graze(..)
        | Stmt::Look(_)
        | Stmt::Signal(_)
        | Stmt::Mark(..)
        | Stmt::Next(..) => false,
    }
}

/// Does a name `f` holds for appear where [`Gen::fold`] would read it?
fn any_name(e: &Expr, f: &impl Fn(&str) -> bool) -> bool {
    match e {
        Expr::Name(n, _) => f(n),
        Expr::Neg(a) => any_name(a, f),
        Expr::Bin(_, a, b) => any_name(a, f) || any_name(b, f),
        Expr::Fn(_, args) => args.iter().any(|a| any_name(a, f)),
        _ => false,
    }
}

/// A statement's line, where it has one.
fn stmt_line(s: &Stmt) -> Option<u32> {
    match s {
        Stmt::Next(_, at) | Stmt::Call { at, .. } => Some(at.line),
        _ => action_at(s).map(|at| at.line),
    }
}

impl<'a> Gen<'a> {
    fn new(items: &'a Items, files: &'a [(&'a str, &'a str)]) -> Self {
        Self {
            files,
            debug: DebugInfo {
                files: files.iter().map(|(n, _)| n.to_string()).collect(),
                ..DebugInfo::default()
            },
            state: None,
            ends_memo: RefCell::default(),
            acts_memo: RefCell::default(),
            in_when: false,
            items: &items.items,
            subs: &items.subs,
            consts: &items.consts,
            const_vals: Vec::new(),
            insts: Vec::new(),
            kind_insts: Vec::new(),
            item_ids: Vec::new(),
            member_index: Vec::new(),
            tags: Vec::new(),
            scents: Vec::new(),
            withs: Vec::new(),
            asm: Asm::new(),
            pool: Vec::new(),
            kind: None,
            cur: None,
            owner: None,
            members_here: Vec::new(),
            sub: None,
            params: Vec::new(),
            checking: false,
            placeholder: false,
            lenient: Vec::new(),
            locals: Vec::new(),
            next_local: 0,
            here: Pos {
                file: String::new(),
                line: 0,
                col: 0,
            },
        }
    }

    fn err(&self, at: &Pos, msg: impl Into<String>) -> CompileError {
        CompileError {
            file: at.file.clone(),
            line: at.line,
            col: at.col,
            msg: msg.into(),
        }
    }

    fn item_named(&self, name: &str) -> Option<usize> {
        self.items.iter().position(|i| i.name == name)
    }

    /// A concrete kind's id (after numbering).
    fn kind_id(&self, name: &str) -> Option<u16> {
        self.item_named(name)
            .and_then(|i| self.item_ids.get(i).copied().flatten())
    }

    /// The id of a concrete kind named in `spawn` or `become`.
    fn concrete(&self, name: &str, at: &Pos) -> Result<u16> {
        match self.kind_id(name) {
            Some(id) => Ok(id),
            None if self.item_named(name).is_some() => Err(self.err(
                at,
                format!("`{name}` is a trait: only a kind can be spawned or become"),
            )),
            None => Err(self.err(at, format!("unknown kind `{name}`"))),
        }
    }

    fn inst_name(&self, i: usize) -> &'a str {
        let items = self.items;
        &items[self.insts[i].item].name
    }

    /// An instance's parameters, bound to its arguments.
    fn scope_of(&self, i: usize) -> Vec<(String, i32)> {
        let inst = &self.insts[i];
        self.items[inst.item]
            .params
            .iter()
            .map(|(n, _)| n.clone())
            .zip(inst.args.iter().copied())
            .collect()
    }

    fn generate(mut self) -> Result<Kinds> {
        let items = self.items;
        // Kind ids and sub indices are u16 (`Kinds` keeps u16::MAX free);
        // checked first, before the passes below that grow with the square.
        if let Some(it) = items.iter().filter(|it| !it.is_trait).nth(MAX_KINDS) {
            return Err(self.err(&it.at, format!("at most {MAX_KINDS} kinds in a rule set")));
        }
        if let Some(s) = self.subs.get(MAX_SUBS) {
            return Err(self.err(&s.at, format!("at most {MAX_SUBS} subs in a rule set")));
        }
        // Names: kinds and traits share one namespace; subs and consts
        // are global too (one namespace across every loaded file).
        for (i, it) in items.iter().enumerate() {
            if let Some(o) = items[..i].iter().find(|o| o.name == it.name) {
                return Err(self.err(
                    &it.at,
                    format!(
                        "{} `{}` declared twice (first at {}:{})",
                        if it.is_trait { "trait" } else { "kind" },
                        it.name,
                        o.at.file,
                        o.at.line
                    ),
                ));
            }
        }
        for (i, s) in self.subs.iter().enumerate() {
            if let Some(o) = self.subs[..i].iter().find(|o| o.name == s.name) {
                return Err(self.err(
                    &s.at,
                    format!(
                        "sub `{}` declared twice (first at {}:{})",
                        s.name, o.at.file, o.at.line
                    ),
                ));
            }
            if let Some(o) = self.item_named(&s.name) {
                let what = if items[o].is_trait { "trait" } else { "kind" };
                return Err(self.err(
                    &s.at,
                    format!(
                        "`{}` is already a {what} (first at {}:{})",
                        s.name, items[o].at.file, items[o].at.line
                    ),
                ));
            }
        }
        // `spawn K ... with (m = a, n = b)`: the names each kind's spawns set.
        let mut sites = Vec::new();
        for it in items {
            let lists = it
                .rules
                .iter()
                .chain(it.states.iter().flat_map(|st| st.rules.iter()));
            for r in lists {
                if let RuleItem::When(rule) = r {
                    spawn_withs(&rule.body, &mut sites);
                }
            }
            for m in &it.members {
                spawn_withs(&m.body, &mut sites);
            }
        }
        for s in self.subs {
            spawn_withs(&s.body, &mut sites);
        }
        for (kind, with) in sites {
            let i = match self.withs.iter().position(|(k, _)| k == kind) {
                Some(i) => i,
                None => {
                    self.withs.push((kind.to_string(), Vec::new()));
                    self.withs.len() - 1
                }
            };
            for (name, _, at) in with {
                let names = &self.withs[i].1;
                if names.iter().any(|(n, _)| n == name) {
                    continue;
                }
                if names.len() == 2 {
                    let all: Vec<&str> = names.iter().map(|(n, _)| n.as_str()).collect();
                    return Err(self.err(
                        at,
                        format!(
                            "spawns of `{kind}` set three memory slots with `with` ({}, {name}): at most two per kind",
                            all.join(", ")
                        ),
                    ));
                }
                self.withs[i].1.push((name.clone(), at.clone()));
            }
        }
        let member_subs = items.iter().flat_map(|it| it.members.iter());
        for s in self.subs.iter().chain(member_subs) {
            let width: usize = s.params.iter().map(|(_, t)| usize::from(t.width())).sum();
            if width > FRAME_LOCALS {
                return Err(self.err(&s.at, format!("sub `{}` has too many parameters", s.name)));
            }
        }
        for it in items {
            for m in &it.members {
                if self.subs.iter().any(|s| s.name == m.name) {
                    return Err(self.err(
                        &m.at,
                        format!(
                            "`{}`'s sub `{}` has the name of a file sub",
                            it.name, m.name
                        ),
                    ));
                }
                if let Some(o) = self.item_named(&m.name) {
                    let what = if items[o].is_trait { "trait" } else { "kind" };
                    return Err(self.err(
                        &m.at,
                        format!(
                            "`{}`'s sub `{}` has the name of a {what} (first at {}:{})",
                            it.name, m.name, items[o].at.file, items[o].at.line
                        ),
                    ));
                }
            }
        }
        // Tags: global names, a bit each, never a kind's or a trait's name.
        for it in items {
            for t in &it.decls.tags {
                if let Some(o) = self.item_named(t) {
                    let what = if items[o].is_trait { "trait" } else { "kind" };
                    return Err(self.err(&it.at, format!("tag `{t}` is also a {what}'s name")));
                }
                if let Some(o) = self.subs.iter().find(|s| s.name == *t) {
                    return Err(self.err(
                        &it.at,
                        format!(
                            "tag `{t}` is also a sub's name (first at {}:{})",
                            o.at.file, o.at.line
                        ),
                    ));
                }
                if !self.tags.contains(t) {
                    if self.tags.len() == MAX_TAGS {
                        return Err(
                            self.err(&it.at, format!("at most {MAX_TAGS} tags in a rule set"))
                        );
                    }
                    self.tags.push(t.clone());
                }
            }
        }
        // Constants: global names, folded in declaration order (a constant
        // may use the ones above it).
        for (i, c) in self.consts.iter().enumerate() {
            let first = |at: &Pos| format!(" (first at {}:{})", at.file, at.line);
            let taken = if let Some(o) = self.consts[..i].iter().find(|o| o.name == c.name) {
                Some(format!("const `{}` declared twice{}", c.name, first(&o.at)))
            } else if let Some(o) = self.item_named(&c.name) {
                let what = if items[o].is_trait { "trait" } else { "kind" };
                Some(format!(
                    "`{}` is already a {what}{}",
                    c.name,
                    first(&items[o].at)
                ))
            } else if let Some(o) = self.subs.iter().find(|s| s.name == c.name) {
                Some(format!("`{}` is already a sub{}", c.name, first(&o.at)))
            } else if self.tags.contains(&c.name) {
                Some(format!("`{}` is already a tag", c.name))
            } else {
                None
            };
            if let Some(msg) = taken {
                return Err(self.err(&c.at, msg));
            }
            let v = self.fold(&c.value, &c.at)?;
            self.const_vals.push((c.name.clone(), v));
        }
        for it in items {
            for (pn, pat) in &it.params {
                if self.const_value(pn).is_some() {
                    return Err(self.err(
                        pat,
                        format!(
                            "parameter `{pn}` of `{}` hides the constant `{pn}`",
                            it.name
                        ),
                    ));
                }
            }
        }

        // Every kind, resolved; then numbered so a family is one id range.
        for (i, it) in items.iter().enumerate() {
            if !it.is_trait {
                self.inst(i, Vec::new(), &mut Vec::new())?;
            }
        }
        self.number_kinds();
        // The sub table: file subs, then each kind's member subs.
        let mut next_sub = self.subs.len();
        let mut sub_names: Vec<String> = self.subs.iter().map(|s| s.name.clone()).collect();
        for k in 0..self.kind_insts.len() {
            let ki = self.kind_insts[k];
            let mut here = Vec::new();
            for (name, ..) in &self.insts[ki].members {
                let idx = u16::try_from(next_sub).map_err(|_| {
                    self.err(
                        &items[self.insts[ki].item].at,
                        format!("at most {MAX_SUBS} subs in a rule set, member subs included"),
                    )
                })?;
                here.push((name.clone(), idx));
                sub_names.push(format!("{}::{name}", self.inst_name(ki)));
                next_sub += 1;
            }
            self.member_index.push(here);
        }
        let mut sub_entries = vec![0u32; next_sub];

        let mut defs = Vec::with_capacity(self.kind_insts.len());
        for k in 0..self.kind_insts.len() {
            defs.push(self.kind_code(k)?);
        }
        for (i, entry) in sub_entries.iter_mut().enumerate().take(self.subs.len()) {
            *entry = self.file_sub_code(i)?;
        }
        for k in 0..self.kind_insts.len() {
            let ki = self.kind_insts[k];
            let members = self.insts[ki].members.clone();
            for ((_, owner, sub), (_, idx)) in members.iter().zip(self.member_index[k].clone()) {
                sub_entries[usize::from(idx)] = self.member_code(k, *owner, sub)?;
            }
        }
        // Traits on their own, whether or not a kind includes them.
        self.check_traits()?;

        // The checks in rule, kind_code and sub_body keep every jump in
        // 16 bits; this is only a backstop.
        let code = std::mem::take(&mut self.asm)
            .try_finish()
            .map_err(|_| self.err(&self.here, "a jump too far for 16 bits: split the rules"))?;
        self.debug.subs = sub_names;
        self.debug.traits = items
            .iter()
            .filter(|i| i.is_trait)
            .map(|i| i.name.clone())
            .collect();
        self.debug.parents = self
            .kind_insts
            .iter()
            .map(|&ki| {
                self.insts[ki]
                    .parents
                    .iter()
                    .map(|&p| {
                        let args = &self.insts[p].args;
                        if args.is_empty() {
                            self.inst_name(p).to_string()
                        } else {
                            let a: Vec<String> = args.iter().map(i32::to_string).collect();
                            format!("{}({})", self.inst_name(p), a.join(", "))
                        }
                    })
                    .collect()
            })
            .collect();
        let kinds = Kinds::from_parts(defs, code, std::mem::take(&mut self.pool), sub_entries)
            .with_scents(std::mem::take(&mut self.scents));
        self.debug.diagnostics = self.lint(&kinds);
        Ok(kinds.with_debug(std::mem::take(&mut self.debug)))
    }

    // ---- inheritance ------------------------------------------------------------------

    /// Resolve `item` with `args` (a trait's arguments; none for a kind):
    /// its ancestors, merged declarations and rule lists. Memoized per
    /// (item, arguments).
    fn inst(&mut self, item: usize, args: Vec<i32>, stack: &mut Vec<usize>) -> Result<usize> {
        if let Some(i) = (0..self.insts.len()).find(|&i| {
            let x = &self.insts[i];
            x.item == item && x.args == args && (self.placeholder || !self.lenient.contains(&i))
        }) {
            return Ok(i);
        }
        let items = self.items;
        let it = &items[item];
        if stack.contains(&item) {
            let chain: Vec<&str> = stack
                .iter()
                .skip_while(|&&i| i != item)
                .map(|&i| items[i].name.as_str())
                .chain([it.name.as_str()])
                .collect();
            return Err(self.err(
                &it.at,
                format!("`{}` extends itself: {}", it.name, chain.join(" -> ")),
            ));
        }
        // Each link is a recursion here and a merge of the whole ancestry:
        // a chain is bounded like a tree's nesting.
        let too_deep = format!(
            "`{}`: a chain of `extends` more than {MAX_DEPTH} kinds and traits deep",
            items[stack.first().copied().unwrap_or(item)].name
        );
        if stack.len() > MAX_DEPTH as usize {
            return Err(self.err(&it.at, too_deep));
        }
        stack.push(item);
        let scope: Vec<(String, i32)> = it
            .params
            .iter()
            .map(|(n, _)| n.clone())
            .zip(args.iter().copied())
            .collect();
        let mut parents = Vec::new();
        let mut concrete: Option<&str> = None;
        for p in &it.parents {
            let Some(pi) = self.item_named(&p.name) else {
                return Err(self.err(&p.at, format!("unknown trait or kind `{}`", p.name)));
            };
            let parent = &items[pi];
            if parent.is_trait {
                if parent.params.len() != p.args.len() {
                    return Err(self.err(
                        &p.at,
                        format!(
                            "trait `{}` takes {} argument{}, {} given",
                            parent.name,
                            parent.params.len(),
                            if parent.params.len() == 1 { "" } else { "s" },
                            p.args.len()
                        ),
                    ));
                }
            } else {
                if it.is_trait {
                    return Err(self.err(
                        &p.at,
                        format!(
                            "trait `{}` can extend only traits: `{}` is a kind",
                            it.name, parent.name
                        ),
                    ));
                }
                if let Some(c) = concrete {
                    return Err(self.err(
                        &p.at,
                        format!(
                            "`{}` extends two kinds, `{c}` and `{}`: a kind extends at most one kind, and any number of traits",
                            it.name, parent.name
                        ),
                    ));
                }
                if !p.args.is_empty() {
                    return Err(
                        self.err(&p.at, format!("kind `{}` takes no arguments", parent.name))
                    );
                }
                concrete = Some(&parent.name);
            }
            let saved = std::mem::replace(&mut self.params, scope.clone());
            let folded: Result<Vec<i32>> = p.args.iter().map(|a| self.fold(a, &p.at)).collect();
            self.params = saved;
            let named = |n: &str| scope.iter().any(|(s, _)| s == n);
            let ph = self.placeholder && p.args.iter().any(|a| any_name(a, &named));
            let was = std::mem::replace(&mut self.placeholder, ph);
            let pinst = folded.and_then(|f| self.inst(pi, f, stack));
            self.placeholder = was;
            parents.push(pinst?);
        }
        stack.pop();
        let depth = parents
            .iter()
            .map(|&p| self.insts[p].depth + 1)
            .max()
            .unwrap_or(0);
        if depth > MAX_DEPTH {
            return Err(self.err(&it.at, too_deep));
        }
        // Linearize: each parent's ancestors, then the parent; the first
        // occurrence wins. One trait, two argument lists: ambiguous (not
        // when this item's arguments are the trait check's placeholder 1s:
        // a kind that really reaches both reports it).
        let mut ancestors: Vec<usize> = Vec::new();
        let mut skipped = false;
        for &p in &parents {
            let chain: Vec<usize> = self.insts[p].ancestors.iter().copied().chain([p]).collect();
            for a in chain {
                if ancestors.contains(&a) {
                    continue;
                }
                if let Some(&o) = ancestors
                    .iter()
                    .find(|&&o| self.insts[o].item == self.insts[a].item)
                {
                    if self.placeholder {
                        skipped = true;
                        continue;
                    }
                    let fmt = |i: usize| -> String {
                        let v: Vec<String> =
                            self.insts[i].args.iter().map(i32::to_string).collect();
                        v.join(", ")
                    };
                    return Err(self.err(
                        &it.at,
                        format!(
                            "`{}` reaches trait `{}` twice, with ({}) and ({})",
                            it.name,
                            self.inst_name(a),
                            fmt(o),
                            fmt(a)
                        ),
                    ));
                }
                ancestors.push(a);
            }
        }
        // Built on a lenient parent: lenient too, so a real lookup re-resolves.
        skipped |= parents.iter().any(|p| self.lenient.contains(p));
        let me = self.insts.len();
        let saved = std::mem::replace(&mut self.params, scope);
        let inst = self.merge(item, args, parents, ancestors, me);
        let inst = inst.map(|i| Inst { depth, ..i });
        self.params = saved;
        let inst = inst?;
        debug_assert_eq!(self.insts.len(), me, "merge resolves nothing new");
        self.insts.push(inst);
        if skipped {
            self.lenient.push(me);
        }
        Ok(me)
    }

    /// A number declared by this item (folded in its scope), checked
    /// against its range (lenient while checking traits: out of range is
    /// "not declared").
    fn decl_num(
        &self,
        v: &Option<(Expr, Pos)>,
        ok: impl Fn(i32) -> bool,
        msg: &str,
    ) -> Result<Option<i32>> {
        let Some((e, at)) = v else {
            return Ok(None);
        };
        let x = self.fold(e, at)?;
        if ok(x) {
            Ok(Some(x))
        } else if self.checking {
            Ok(None)
        } else {
            Err(self.err(at, msg))
        }
    }

    /// The value an item gets for one declaration: its own, else the one its
    /// direct parents agree on.
    fn pick<T: Copy + PartialEq>(
        &self,
        own: Option<T>,
        parents: &[usize],
        get: impl Fn(&Inst<'a>) -> Option<T>,
        what: &str,
        item: usize,
    ) -> Result<Option<T>> {
        if own.is_some() {
            return Ok(own);
        }
        let mut found: Option<(usize, T)> = None;
        for &p in parents {
            if let Some(v) = get(&self.insts[p]) {
                match found {
                    None => found = Some((p, v)),
                    Some((q, w)) if w != v => {
                        let it = &self.items[item];
                        return Err(self.err(
                            &it.at,
                            format!(
                                "`{}` inherits different {what} from `{}` and `{}`: declare it in `{}`",
                                it.name,
                                self.inst_name(q),
                                self.inst_name(p),
                                it.name
                            ),
                        ));
                    }
                    _ => {}
                }
            }
        }
        Ok(found.map(|(_, v)| v))
    }

    /// Merge an item's own declarations over its parents' (merged) ones
    /// and resolve its rule lists. `self.params` is the item's scope.
    fn merge(
        &mut self,
        item: usize,
        args: Vec<i32>,
        parents: Vec<usize>,
        ancestors: Vec<usize>,
        me: usize,
    ) -> Result<Inst<'a>> {
        let items = self.items;
        let it = &items[item];
        let d = &it.decls;
        let cadence = self
            .decl_num(
                &d.cadence,
                |c| c >= 1 && (c as u32).is_power_of_two(),
                "cadence must be a power of two (1, 2, 4, ...)",
            )?
            .map(|c| u8::try_from(c.trailing_zeros()).expect("a positive i32: < 31"));
        let sight = self
            .decl_num(
                &d.sight,
                |s| (0..=i32::from(MAX_SIGHT)).contains(&s),
                &format!("sight is 0 to {MAX_SIGHT} cells"),
            )?
            .map(|s| u8::try_from(s).expect("0..=MAX_SIGHT, checked above"));
        let fuel = self
            .decl_num(
                &d.fuel,
                |f| (1..=MAX_FUEL as i32).contains(&f),
                &format!("fuel is 1 to {MAX_FUEL} ops per think"),
            )?
            .map(|f| f as u32);
        let food = self.decl_num(&d.food, |f| f >= 0, "food is 0 or more ticks")?;
        let bite = self
            .decl_num(&d.bite, |b| (0..=255).contains(&b), "bite is 0 to 255")?
            .map(|b| u8::try_from(b).expect("0..=255, checked above"));
        let glyph = self.pick(d.glyph, &parents, |i| i.glyph, "glyphs", item)?;
        let color = self.pick(d.color, &parents, |i| i.color, "colours", item)?;
        let cadence_shift = self.pick(cadence, &parents, |i| i.cadence_shift, "cadences", item)?;
        let sight = self.pick(sight, &parents, |i| i.sight, "sights", item)?;
        let fuel = self.pick(fuel, &parents, |i| i.fuel, "fuel budgets", item)?;
        let food = self.pick(food, &parents, |i| i.food, "food values", item)?;
        let bite = self.pick(bite, &parents, |i| i.bite, "bites", item)?;
        let cover = d.cover || parents.iter().any(|&p| self.insts[p].cover);

        let mut tags: Vec<String> = Vec::new();
        for t in parents
            .iter()
            .flat_map(|&p| self.insts[p].tags.iter())
            .chain(&d.tags)
        {
            if !tags.contains(t) {
                tags.push(t.clone());
            }
        }

        // Needs by name, parents' slots first; a redeclaration overrides in
        // place. Parents that disagree must be settled by the item.
        let mut needs: Vec<NeedDef> = Vec::new();
        let mut from: Vec<usize> = Vec::new();
        let mut clashes: Vec<(String, usize, usize)> = Vec::new();
        for &p in &parents {
            for n in &self.insts[p].needs {
                match needs.iter().position(|x| x.name == n.name) {
                    None => {
                        needs.push(n.clone());
                        from.push(p);
                    }
                    Some(i) if needs[i] == *n => {}
                    Some(i) => clashes.push((n.name.clone(), from[i], p)),
                }
            }
        }
        for na in &d.needs {
            let max = self.fold(&na.max, &na.at)?;
            if max < 1 && !self.checking {
                return Err(self.err(&na.at, "a need's max is at least 1"));
            }
            let def = NeedDef {
                name: na.name.clone(),
                max: max.max(1),
                decays: na.decays,
                vital: na.vital,
            };
            match needs.iter().position(|x| x.name == def.name) {
                Some(i) => needs[i] = def,
                None => needs.push(def),
            }
        }
        if let Some((n, a, b)) = clashes
            .iter()
            .find(|(n, ..)| !d.needs.iter().any(|x| x.name == *n))
        {
            return Err(self.err(
                &it.at,
                format!(
                    "`{}` inherits need `{n}` from `{}` and `{}`, declared differently: redeclare it in `{}`",
                    it.name,
                    self.inst_name(*a),
                    self.inst_name(*b),
                    it.name
                ),
            ));
        }
        if needs.len() > NEED_SLOTS {
            let names: Vec<&str> = needs.iter().map(|n| n.name.as_str()).collect();
            return Err(self.err(
                &it.at,
                format!(
                    "`{}` has {} needs, at most {NEED_SLOTS}: {}",
                    it.name,
                    needs.len(),
                    names.join(", ")
                ),
            ));
        }
        let mut mems: Vec<String> = Vec::new();
        for m in parents.iter().flat_map(|&p| self.insts[p].mems.iter()) {
            if !mems.contains(m) {
                mems.push(m.clone());
            }
        }
        for (m, at) in &d.mems {
            if needs.iter().any(|n| n.name == *m) {
                return Err(self.err(at, format!("`{m}` is a need of `{}` already", it.name)));
            }
            if !mems.contains(m) {
                mems.push(m.clone());
            }
        }
        if let Some(m) = mems.iter().find(|m| needs.iter().any(|n| n.name == **m)) {
            return Err(self.err(
                &it.at,
                format!(
                    "`{}` inherits `{m}` both as a need and as a mem slot",
                    it.name
                ),
            ));
        }
        if mems.len() > MEM_SLOTS {
            return Err(self.err(
                &it.at,
                format!(
                    "`{}` has {} mem slots, at most {MEM_SLOTS}: {}",
                    it.name,
                    mems.len(),
                    mems.join(", ")
                ),
            ));
        }
        // The slots a `spawn ... with` sets come first: a spawn's two values
        // land in slots 0 and 1, whatever the kind inherits.
        if !it.is_trait
            && let Some((_, names)) = self.withs.iter().find(|(k, _)| *k == it.name)
        {
            for (i, (name, at)) in names.iter().enumerate() {
                let Some(from) = mems.iter().position(|m| m == name) else {
                    return Err(self.err(
                        at,
                        format!(
                            "`spawn {} ... with`: `{}` has no memory `{name}`",
                            it.name, it.name
                        ),
                    ));
                };
                let m = mems.remove(from);
                mems.insert(i, m);
            }
        }
        for (pn, pat) in &it.params {
            if needs.iter().any(|n| n.name == *pn) || mems.contains(pn) {
                return Err(self.err(
                    pat,
                    format!(
                        "parameter `{pn}` has the name of a need or mem slot of `{}`",
                        it.name
                    ),
                ));
            }
        }

        let mut states: Vec<String> = Vec::new();
        for s in parents
            .iter()
            .flat_map(|&p| self.insts[p].states.iter())
            .chain(it.states.iter().map(|s| &s.name))
        {
            if !states.contains(s) {
                states.push(s.clone());
            }
        }
        if states.len() > MAX_STATES {
            return Err(self.err(
                &it.at,
                format!("`{}` has more than {MAX_STATES} states", it.name),
            ));
        }

        // Member subs by name; the item's own override its parents'.
        let mut members: Vec<(String, usize, &'a SubAst)> = Vec::new();
        let mut sub_clashes: Vec<(String, usize, usize)> = Vec::new();
        for &p in &parents {
            for (n, o, sub) in &self.insts[p].members {
                match members.iter().position(|m| m.0 == *n) {
                    None => members.push((n.clone(), *o, sub)),
                    Some(i) if members[i].1 == *o => {}
                    Some(i) => sub_clashes.push((n.clone(), members[i].1, *o)),
                }
            }
        }
        for sub in &it.members {
            match members.iter().position(|m| m.0 == sub.name) {
                Some(i) => members[i] = (sub.name.clone(), me, sub),
                None => members.push((sub.name.clone(), me, sub)),
            }
        }
        if let Some((n, a, b)) = sub_clashes
            .iter()
            .find(|(n, ..)| !it.members.iter().any(|m| m.name == *n))
        {
            return Err(self.err(
                &it.at,
                format!(
                    "`{}` inherits sub `{n}` from both `{}` and `{}`: define it in `{}`",
                    it.name,
                    self.inst_name(*a),
                    self.inst_name(*b),
                    it.name
                ),
            ));
        }

        let reflex = self.resolve_list(me, item, &parents, &ancestors, &it.rules, None)?;
        let mut state_lists = Vec::with_capacity(states.len());
        for sname in &states {
            let own: &'a [RuleItem] = it
                .states
                .iter()
                .find(|s| s.name == *sname)
                .map_or(&[][..], |s| &s.rules[..]);
            state_lists.push(self.resolve_list(
                me,
                item,
                &parents,
                &ancestors,
                own,
                Some(sname),
            )?);
        }
        Ok(Inst {
            item,
            args,
            parents,
            ancestors,
            depth: 0,
            glyph,
            color,
            cover,
            cadence_shift,
            sight,
            fuel,
            food,
            bite,
            tags,
            needs,
            mems,
            states,
            members,
            reflex,
            state_lists,
        })
    }

    /// One rule list of the instance `me` (being resolved): its own rules
    /// in order, `inherit` splicing ancestors' lists where it stands, and,
    /// if the list has no `inherit` at all, the direct parents' lists
    /// appended. `state`: which list (`None`: the reflexes).
    fn resolve_list(
        &self,
        me: usize,
        item: usize,
        parents: &[usize],
        ancestors: &[usize],
        own: &'a [RuleItem],
        state: Option<&str>,
    ) -> Result<RList<'a>> {
        let list_of = |i: usize| -> Option<&RList<'a>> {
            match state {
                None => Some(&self.insts[i].reflex),
                Some(s) => self.insts[i].state_list(s),
            }
        };
        let splice = |out: &mut RList<'a>, l: &RList<'a>| {
            let new: Vec<usize> = l
                .sources
                .iter()
                .copied()
                .filter(|s| !out.sources.contains(s))
                .collect();
            out.rules
                .extend(l.rules.iter().filter(|r| new.contains(&r.owner)).copied());
            out.sources.extend(new);
        };
        let mut out = RList {
            rules: Vec::new(),
            sources: vec![me],
        };
        let mut named: Vec<usize> = Vec::new();
        let explicit = own.iter().any(|r| matches!(r, RuleItem::Inherit(..)));
        for r in own {
            match r {
                RuleItem::When(rule) => out.rules.push(RRule { owner: me, rule }),
                RuleItem::Inherit(None, _) => {
                    for &p in parents {
                        if let Some(l) = list_of(p) {
                            splice(&mut out, l);
                        }
                    }
                }
                RuleItem::Inherit(Some(n), at) => {
                    let Some(&a) = ancestors.iter().find(|&&a| self.inst_name(a) == n) else {
                        return Err(self.err(
                            at,
                            format!("`{n}` is not an ancestor of `{}`", self.items[item].name),
                        ));
                    };
                    if named.contains(&a) {
                        return Err(self.err(at, format!("`inherit {n}` twice in one list")));
                    }
                    named.push(a);
                    match list_of(a) {
                        Some(l) => splice(&mut out, l),
                        None => {
                            return Err(self.err(
                                at,
                                format!("`{n}` has no state `{}`", state.unwrap_or_default()),
                            ));
                        }
                    }
                }
            }
        }
        if !explicit {
            for &p in parents {
                if let Some(l) = list_of(p) {
                    splice(&mut out, l);
                }
            }
        }
        Ok(out)
    }

    /// Number the concrete kinds in pre-order over the inheritance forest:
    /// roots in file then declaration order, each kind's children right
    /// after it in the same order. A family is then one id range.
    fn number_kinds(&mut self) {
        let items = self.items;
        let concrete_parent = |i: usize| -> Option<usize> {
            items[i].parents.iter().find_map(|p| {
                items
                    .iter()
                    .position(|o| o.name == p.name)
                    .filter(|&pi| !items[pi].is_trait)
            })
        };
        let kinds: Vec<usize> = (0..items.len()).filter(|&i| !items[i].is_trait).collect();
        let mut order = Vec::with_capacity(kinds.len());
        let mut stack: Vec<usize> = kinds
            .iter()
            .rev()
            .copied()
            .filter(|&k| concrete_parent(k).is_none())
            .collect();
        while let Some(k) = stack.pop() {
            order.push(k);
            stack.extend(
                kinds
                    .iter()
                    .rev()
                    .copied()
                    .filter(|&c| concrete_parent(c) == Some(k)),
            );
        }
        self.item_ids = vec![None; items.len()];
        for (id, &k) in order.iter().enumerate() {
            self.item_ids[k] = Some(u16::try_from(id).expect("checked in generate"));
        }
        self.kind_insts = order
            .iter()
            .map(|&k| {
                self.insts
                    .iter()
                    .position(|x| x.item == k && x.args.is_empty())
                    .expect("every kind was resolved")
            })
            .collect();
    }

    /// Compile in the context of kind `k`: its tables and member subs.
    fn enter(&mut self, k: usize) {
        self.kind = Some(u16::try_from(k).expect("checked in generate"));
        self.cur = Some(self.kind_insts[k]);
        self.members_here = self.member_index[k].clone();
    }

    fn kind_code(&mut self, k: usize) -> Result<KindDef> {
        self.enter(k);
        self.sub = None;
        let ki = self.kind_insts[k];
        let inst = self.insts[ki].clone();
        let it = &self.items[inst.item];
        self.here = it.at.clone();
        let entry = self.asm.here();
        self.state = None;
        self.rule_list(&inst.reflex)?;
        // States: the current one's rules after the reflexes. An actor
        // starts in the first; a state out of range runs no rules.
        self.debug.states.push(inst.states.clone());
        for (s, list) in inst.state_lists.iter().enumerate() {
            let state = u8::try_from(s).expect("at most MAX_STATES states");
            self.state = Some(state);
            let skip = self.asm.label();
            let guard_pc = self.asm.here();
            self.asm
                .sense(Sense::State)
                .push(i32::from(state))
                .op(OpCode::Eq)
                .jz(skip);
            self.rule_list(list)?;
            self.asm.halt().bind(skip);
            // The guard's jump skips the whole state: 16 bits.
            if self.asm.here() - guard_pc > i16::MAX as u32 {
                let name = &inst.states[s];
                let at = std::iter::once(inst.item)
                    .chain(inst.ancestors.iter().map(|&a| self.insts[a].item))
                    .find_map(|i| self.items[i].states.iter().find(|st| st.name == *name))
                    .map_or(&it.at, |st| &st.at);
                return Err(self.err(
                    at,
                    format!(
                        "state `{name}` of `{}` compiles to more than 32767 ops: split it",
                        it.name
                    ),
                ));
            }
        }
        self.asm.halt();
        let tags = inst.tags.iter().fold(0u64, |bits, t| {
            bits | 1
                << self
                    .tags
                    .iter()
                    .position(|x| x == t)
                    .expect("collected above")
        });
        let parent = inst
            .parents
            .iter()
            .find_map(|&p| self.item_ids[self.insts[p].item]);
        Ok(KindDef {
            id: u16::try_from(k).expect("checked in generate"),
            name: it.name.clone(),
            glyph: inst.glyph.unwrap_or(b'?'),
            tags,
            cadence_shift: inst.cadence_shift.unwrap_or(3),
            sight: inst.sight.unwrap_or(4),
            fuel: inst.fuel.unwrap_or(512),
            food: inst.food.unwrap_or(0),
            bite: inst.bite.unwrap_or(1),
            needs: inst.needs.clone(),
            mems: inst.mems.clone(),
            states: u8::try_from(inst.states.len().max(1)).expect("at most MAX_STATES states"),
            entry,
            color: inst.color.unwrap_or(DEFAULT_COLOR),
            cover: inst.cover,
            parent,
        })
    }

    fn file_sub_code(&mut self, i: usize) -> Result<u32> {
        let subs = self.subs;
        let s = &subs[i];
        self.kind = None;
        self.cur = None;
        self.owner = None;
        self.members_here.clear();
        self.params.clear();
        self.sub_body(s)
    }

    fn member_code(&mut self, k: usize, owner: usize, s: &'a SubAst) -> Result<u32> {
        self.enter(k);
        self.owner = Some(owner);
        self.params = self.scope_of(owner);
        self.sub_body(s)
    }

    /// A sub's code: its parameters as the first locals, the body, a return.
    fn sub_body(&mut self, s: &'a SubAst) -> Result<u32> {
        self.sub = Some(s);
        self.here = s.at.clone();
        let entry = self.asm.here();
        self.locals.clear();
        self.next_local = 0;
        for (name, ty) in &s.params {
            self.hides(name, &format!("parameter `{name}` of `{}`", s.name), &s.at)?;
            let slot = self.alloc_local(&s.at, ty.width().into())?;
            self.locals.push(Local {
                name: name.clone(),
                slot,
                ty: *ty,
            });
        }
        self.stmts(&s.body)?;
        // Falling off the end: a function returns 0, a procedure nothing.
        if s.returns {
            self.asm.push(0).ret(true);
        } else {
            self.asm.ret(false);
        }
        if self.asm.here() - entry > i16::MAX as u32 {
            return Err(self.err(
                &s.at,
                format!("sub `{}` compiles to more than 32767 ops: split it", s.name),
            ));
        }
        let peak = self.asm.take_peak();
        if peak as usize > STACK {
            return Err(self.err(
                &s.at,
                format!(
                    "sub `{}` needs {peak} stack values; the VM has {STACK}: nest less deeply or split the expression with `let`",
                    s.name
                ),
            ));
        }
        self.sub = None;
        Ok(entry)
    }

    /// Compile every trait on its own, each parameter bound to 1, and keep
    /// nothing: a trait names only what it declares, so this proves it
    /// compiles for any kind that includes it, whether any kind does.
    fn check_traits(&mut self) -> Result<()> {
        let saved_asm = std::mem::take(&mut self.asm);
        let marks = (self.pool.len(), self.scents.len(), self.debug.rules.len());
        let was = self.checking;
        self.checking = true;
        let r = self.check_all_traits();
        self.asm = saved_asm;
        self.pool.truncate(marks.0);
        self.scents.truncate(marks.1);
        self.debug.rules.truncate(marks.2);
        self.checking = was;
        self.kind = None;
        self.cur = None;
        self.owner = None;
        self.sub = None;
        r
    }

    fn check_all_traits(&mut self) -> Result<()> {
        let items = self.items;
        for (i, it) in items.iter().enumerate() {
            if !it.is_trait {
                continue;
            }
            self.placeholder = !it.params.is_empty();
            let ti = self.inst(i, vec![1; it.params.len()], &mut Vec::new());
            self.placeholder = false;
            let ti = ti?;
            let inst = self.insts[ti].clone();
            self.kind = None;
            self.cur = Some(ti);
            self.members_here = inst.members.iter().map(|(n, ..)| (n.clone(), 0)).collect();
            self.sub = None;
            self.state = None;
            let own = |l: &RList<'a>| -> RList<'a> {
                RList {
                    rules: l.rules.iter().filter(|r| r.owner == ti).copied().collect(),
                    sources: vec![ti],
                }
            };
            self.rule_list(&own(&inst.reflex))?;
            for (s, list) in inst.state_lists.iter().enumerate() {
                self.state = Some(u8::try_from(s).expect("at most MAX_STATES states"));
                self.rule_list(&own(list))?;
            }
            for (_, owner, sub) in inst.members.iter().filter(|m| m.1 == ti) {
                self.owner = Some(*owner);
                self.params = self.scope_of(*owner);
                self.sub_body(sub)?;
            }
        }
        Ok(())
    }

    /// Does this statement end the think on every path: an action (and, if
    /// not `acts_only`, a `next`)? Conservative: loops never count, a call
    /// counts only through subs that do, eight calls deep, and a `choose`
    /// weight only if it is a constant nothing here may hide (`lets`: a
    /// `let` of this body is in scope; inside a callee, its parameters are).
    fn ends(&self, s: &Stmt, acts_only: bool, depth: u32, lets: bool) -> bool {
        match s {
            Stmt::Next(..) => !acts_only,
            Stmt::If { then, els, .. } => {
                !els.is_empty()
                    && self.ends_all(then, acts_only, depth, lets)
                    && self.ends_all(els, acts_only, depth, lets)
            }
            Stmt::Choose(arms) => {
                !arms.is_empty()
                    && arms.iter().all(|(w, body)| {
                        self.fold_known(w, &self.here, |_| lets || depth > 0)
                            .is_some_and(|w| w > 0)
                            && self.ends_all(body, acts_only, depth, lets)
                    })
            }
            Stmt::Call { name, .. } => {
                // A callee's answer depends only on it, the tables and the
                // depth (inside it every name is unknown): remembered.
                (depth as usize) < FRAMES
                    && self.callee(name).is_some_and(|sub| {
                        let key = (std::ptr::from_ref(sub) as usize, self.cur, depth, acts_only);
                        if let Some(&v) = self.ends_memo.borrow().get(&key) {
                            return v;
                        }
                        let v = self.ends_all(&sub.body, acts_only, depth + 1, false);
                        self.ends_memo.borrow_mut().insert(key, v);
                        v
                    })
            }
            _ => action_at(s).is_some(),
        }
    }

    /// In order: a statement that ends the think before any that may
    /// `return` out of the sub.
    fn ends_all(&self, body: &[Stmt], acts_only: bool, depth: u32, lets: bool) -> bool {
        let lets = lets || body.iter().any(|s| matches!(s, Stmt::Let { .. }));
        for s in body {
            if self.ends(s, acts_only, depth, lets) {
                return true;
            }
            if may_return(s) {
                return false;
            }
        }
        false
    }

    /// The sub a call named `name` reaches here: a member sub of the
    /// current tables, else a file sub.
    fn callee(&self, name: &str) -> Option<&'a SubAst> {
        if let Some(cur) = self.cur
            && let Some((_, _, s)) = self.insts[cur].members.iter().find(|(n, ..)| n == name)
        {
            return Some(*s);
        }
        let subs = self.subs;
        subs.iter().find(|s| s.name == name)
    }

    /// May a call of `name` here act or `next`, on any path? Follows every
    /// call in its body, in statements and in expressions, eight deep.
    fn call_acts(&self, name: &str, depth: u32) -> bool {
        (depth as usize) < FRAMES
            && self.callee(name).is_some_and(|sub| {
                let key = (std::ptr::from_ref(sub) as usize, self.cur, depth);
                if let Some(&v) = self.acts_memo.borrow().get(&key) {
                    return v;
                }
                let v = sub.body.iter().any(|s| self.stmt_acts(s, depth + 1));
                self.acts_memo.borrow_mut().insert(key, v);
                v
            })
    }

    fn stmt_acts(&self, s: &Stmt, depth: u32) -> bool {
        let body = |b: &[Stmt]| b.iter().any(|s| self.stmt_acts(s, depth));
        match s {
            Stmt::Next(..) => true,
            _ if action_at(s).is_some() => true,
            Stmt::Set { value, .. } | Stmt::Assign { value, .. } | Stmt::Let { value, .. } => {
                self.expr_acts(value, depth)
            }
            Stmt::Look(e) | Stmt::Signal(e) | Stmt::Mark(_, e, _) => self.expr_acts(e, depth),
            Stmt::Return { value, .. } => value.as_ref().is_some_and(|e| self.expr_acts(e, depth)),
            Stmt::If { cond, then, els } => self.cond_acts(cond, depth) || body(then) || body(els),
            Stmt::While { cond, body: b } => self.cond_acts(cond, depth) || body(b),
            Stmt::Repeat { count, body: b } => self.expr_acts(count, depth) || body(b),
            Stmt::ForEach { r, body: b, .. } => self.expr_acts(r, depth) || body(b),
            Stmt::Choose(arms) => arms
                .iter()
                .any(|(w, b)| self.expr_acts(w, depth) || body(b)),
            Stmt::Call { name, args, .. } => {
                self.args_act(args, depth) || self.call_acts(name, depth)
            }
            _ => false,
        }
    }

    fn cond_acts(&self, c: &Cond, depth: u32) -> bool {
        match c {
            Cond::Expr(e) | Cond::Nearest { r: e, .. } | Cond::Sniff { r: e, .. } => {
                self.expr_acts(e, depth)
            }
            Cond::And(a, b) | Cond::Or(a, b) => {
                self.cond_acts(a, depth) || self.cond_acts(b, depth)
            }
            Cond::Not(a) => self.cond_acts(a, depth),
        }
    }

    fn expr_acts(&self, e: &Expr, depth: u32) -> bool {
        match e {
            Expr::Neg(a) | Expr::Rand(a) | Expr::Chance(a) | Expr::Count(_, a) => {
                self.expr_acts(a, depth)
            }
            Expr::Bin(_, a, b) => self.expr_acts(a, depth) || self.expr_acts(b, depth),
            Expr::Fn(_, args) => args.iter().any(|a| self.expr_acts(a, depth)),
            Expr::Dist(t)
            | Expr::FreeAt(t)
            | Expr::IsAt(t, _)
            | Expr::LookOf(t)
            | Expr::SignalOf(t, _)
            | Expr::Scent(_, Some(t), _) => self.target_acts(t, depth),
            Expr::Call { name, args, .. } => {
                self.args_act(args, depth) || self.call_acts(name, depth)
            }
            Expr::Int(_) | Expr::Name(..) | Expr::Field(..) | Expr::Sense(_) | Expr::Scent(..) => {
                false
            }
        }
    }

    fn target_acts(&self, t: &Target, depth: u32) -> bool {
        match t {
            Target::Heading(e) => self.expr_acts(e, depth),
            Target::At(a, b) => self.expr_acts(a, depth) || self.expr_acts(b, depth),
            Target::Toward(t) | Target::Away(t) => self.target_acts(t, depth),
            _ => false,
        }
    }

    fn args_act(&self, args: &[Arg], depth: u32) -> bool {
        args.iter().any(|a| match a {
            Arg::Expr(e) => self.expr_acts(e, depth),
            Arg::Target(t) => self.target_acts(t, depth),
            Arg::Pred(_) | Arg::Name(..) => false,
        })
    }

    fn rule_list(&mut self, list: &RList<'a>) -> Result<()> {
        for rr in &list.rules {
            self.owner = Some(rr.owner);
            self.params = self.scope_of(rr.owner);
            self.rule(rr.rule)?;
        }
        self.owner = self.cur;
        Ok(())
    }

    fn rule(&mut self, rule: &'a Rule) -> Result<()> {
        self.here = rule.at.clone();
        self.locals.clear();
        self.next_local = 0;
        let next = self.asm.label();
        let cond_pc = self.asm.here();
        self.in_when = true;
        self.cond(&rule.cond, next, true)?;
        self.in_when = false;
        let body_pc = self.asm.here();
        self.stmts(&rule.body)?;
        self.asm.end_rule().bind(next);
        // Its jumps are 16 bits.
        if self.asm.here() - cond_pc > i16::MAX as u32 {
            return Err(self.err(
                &rule.at,
                "rule body too long: it compiles to more than 32767 ops; split it",
            ));
        }
        let peak = self.asm.take_peak();
        if peak as usize > STACK {
            return Err(self.err(
                &rule.at,
                format!(
                    "this rule needs {peak} stack values; the VM has {STACK}: nest less deeply or split the expression with `let`"
                ),
            ));
        }
        let file = self
            .files
            .iter()
            .position(|(n, _)| *n == rule.at.file)
            .unwrap_or(0);
        // From `when` to the line of `=>`, comments dropped, one line.
        let from = rule.at.col.saturating_sub(1) as usize;
        let text = self.files.get(file).map_or(String::new(), |(_, t)| {
            t.lines()
                .skip(rule.at.line.saturating_sub(1) as usize)
                .take((rule.arrow_line.max(rule.at.line) - rule.at.line + 1) as usize)
                .enumerate()
                .map(|(i, l)| {
                    let l = if i == 0 {
                        l.get(from..).unwrap_or(l)
                    } else {
                        l
                    };
                    l.split('#').next().unwrap_or("").trim()
                })
                .collect::<Vec<_>>()
                .join(" ")
        });
        let via = match (self.owner, self.cur) {
            (Some(o), Some(c)) if o != c => Some(self.inst_name(o).to_string()),
            _ => None,
        };
        self.debug.rules.push(RuleInfo {
            kind: self.kind.unwrap_or(0),
            state: self.state,
            file: u16::try_from(file).expect("at most MAX_FILES files"),
            line: rule.at.line,
            text,
            cond_pc,
            body_pc,
            via,
        });
        Ok(())
    }

    /// The channel of scent `name`, numbered on first use.
    fn scent(&mut self, name: &str, at: &Pos) -> Result<u8> {
        if let Some(i) = self.scents.iter().position(|s| s == name) {
            return Ok(u8::try_from(i).expect("at most SCENT_CHANNELS scents"));
        }
        if self.scents.len() == SCENT_CHANNELS {
            return Err(self.err(
                at,
                format!(
                    "at most {SCENT_CHANNELS} scents in a rule set ({} and `{name}`)",
                    self.scents.join(", ")
                ),
            ));
        }
        self.scents.push(name.to_string());
        Ok(u8::try_from(self.scents.len() - 1).expect("at most SCENT_CHANNELS scents"))
    }

    fn const_value(&self, n: &str) -> Option<i32> {
        self.const_vals
            .iter()
            .find(|(c, _)| c == n)
            .map(|&(_, v)| v)
    }

    /// Fold a `const` expression: numbers, other constants, arithmetic,
    /// comparisons and the pure functions. Same results as the VM
    /// (`folded_constants_match_the_vm` checks). `at`: where to report
    /// anything else.
    fn fold(&self, e: &Expr, at: &Pos) -> Result<i32> {
        Ok(match e {
            Expr::Int(v) => *v,
            Expr::Name(n, at) => self
                .param(n)
                .or_else(|| self.const_value(n))
                .ok_or_else(|| self.err(at, format!("`{n}` is not a constant declared above")))?,
            Expr::Neg(a) => self.fold(a, at)?.wrapping_neg(),
            Expr::Bin(op, a, b) => {
                let (x, y) = (self.fold(a, at)?, self.fold(b, at)?);
                match op {
                    OpCode::Add => x.wrapping_add(y),
                    OpCode::Sub => x.wrapping_sub(y),
                    OpCode::Mul => x.wrapping_mul(y),
                    OpCode::Div if y == 0 => 0,
                    OpCode::Div => x.wrapping_div(y),
                    OpCode::Mod if y == 0 => 0,
                    OpCode::Mod => x.wrapping_rem(y),
                    OpCode::Lt => i32::from(x < y),
                    OpCode::Le => i32::from(x <= y),
                    OpCode::Eq => i32::from(x == y),
                    OpCode::Ne => i32::from(x != y),
                    OpCode::Ge => i32::from(x >= y),
                    OpCode::Gt => i32::from(x > y),
                    op => unreachable!("{op:?} is not an operator"),
                }
            }
            Expr::Fn(op, args) => {
                let v = args
                    .iter()
                    .map(|a| self.fold(a, at))
                    .collect::<Result<Vec<_>>>()?;
                match op {
                    OpCode::Min => v[0].min(v[1]),
                    OpCode::Max => v[0].max(v[1]),
                    OpCode::Abs => v[0].wrapping_abs(),
                    OpCode::Sign => v[0].signum(),
                    OpCode::Clamp if v[1] <= v[2] => v[0].clamp(v[1], v[2]),
                    OpCode::Clamp => v[1],
                    OpCode::Pack => v[0].wrapping_mul(256).wrapping_add(v[1] & 0xFF),
                    OpCode::Hi => v[0] >> 8,
                    OpCode::Lo => (v[0] << 24) >> 24,
                    op => unreachable!("{op:?} is not a pure function"),
                }
            }
            _ => {
                return Err(self.err(
                    at,
                    "a constant must be a number, other constants and arithmetic",
                ));
            }
        })
    }

    /// [`Self::fold`] for the analyses, which may only warn or refuse
    /// less: `None` if a name in `e` may not be the constant. A local, a
    /// need or a mem of that name hides it, and so does any name `hidden`
    /// says may be one.
    fn fold_known(&self, e: &Expr, at: &Pos, hidden: impl Fn(&str) -> bool) -> Option<i32> {
        let shadows = |n: &str| {
            hidden(n)
                || self.local(n).is_some()
                || self.cur.is_some_and(|c| {
                    let i = &self.insts[c];
                    i.needs.iter().any(|d| d.name == n) || i.mems.iter().any(|m| m == n)
                })
        };
        if any_name(e, &shadows) {
            return None;
        }
        self.fold(e, at).ok()
    }

    fn push_int(&mut self, v: i32) -> Result<()> {
        if let Ok(imm) = i16::try_from(v) {
            self.asm.push(i32::from(imm));
        } else {
            let idx = match self.pool.iter().position(|&c| c == v) {
                Some(i) => i,
                None => {
                    self.pool.push(v);
                    self.pool.len() - 1
                }
            };
            let Ok(idx) = u16::try_from(idx) else {
                return Err(self.err(
                    &self.here,
                    format!("too many distinct constants outside -32768..32767 (max {MAX_POOL})"),
                ));
            };
            self.asm.push_k(idx);
        }
        Ok(())
    }

    fn alloc_local(&mut self, at: &Pos, n: usize) -> Result<u8> {
        let slot = self.next_local;
        let end = usize::from(slot) + n;
        if end > FRAME_LOCALS {
            return Err(self.err(
                at,
                format!(
                    "too many bindings and locals in one rule or sub (max {FRAME_LOCALS} slots)"
                ),
            ));
        }
        self.next_local = u8::try_from(end).expect("at most FRAME_LOCALS");
        Ok(slot)
    }

    fn local(&self, name: &str) -> Option<&Local> {
        self.locals.iter().rev().find(|l| l.name == name)
    }

    /// The slot of need `n` in the tables being compiled, if the code's
    /// owner may name it (a trait sees only what it or its ancestors
    /// declare).
    fn need_slot(&self, n: &str) -> Option<u8> {
        let (cur, owner) = (self.cur?, self.owner?);
        if !self.insts[owner].needs.iter().any(|d| d.name == n) {
            return None;
        }
        self.insts[cur]
            .needs
            .iter()
            .position(|d| d.name == n)
            .map(|i| u8::try_from(i).expect("at most NEED_SLOTS needs"))
    }

    fn mem_slot(&self, n: &str) -> Option<u8> {
        let (cur, owner) = (self.cur?, self.owner?);
        if !self.insts[owner].mems.iter().any(|m| m == n) {
            return None;
        }
        self.insts[cur]
            .mems
            .iter()
            .position(|m| m == n)
            .map(|i| u8::try_from(i).expect("at most MEM_SLOTS mems"))
    }

    /// A local, binding or parameter (`what`) named `n` would hide a need
    /// or mem slot this code sees: refused, as for a trait parameter.
    fn hides(&self, n: &str, what: &str, at: &Pos) -> Result<()> {
        match self.owner {
            Some(o) if self.need_slot(n).is_some() || self.mem_slot(n).is_some() => Err(self.err(
                at,
                format!(
                    "{what} has the name of a need or mem slot of `{}`",
                    self.inst_name(o)
                ),
            )),
            _ => Ok(()),
        }
    }

    /// A trait parameter in scope.
    fn param(&self, n: &str) -> Option<i32> {
        self.params.iter().find(|(p, _)| p == n).map(|&(_, v)| v)
    }

    fn unknown_name(&self, at: &Pos, n: &str) -> CompileError {
        if let (Some(cur), Some(owner)) = (self.cur, self.owner)
            && cur != owner
        {
            let c = &self.insts[cur];
            if c.needs.iter().any(|d| d.name == n) || c.mems.iter().any(|m| m == n) {
                return self.err(
                    at,
                    format!(
                        "`{}` uses `{n}`, which it does not declare (a trait or parent kind sees only its own needs and mems)",
                        self.inst_name(owner)
                    ),
                );
            }
        }
        if self.cur.is_none() {
            self.err(
                at,
                format!("unknown name `{n}` (a sub sees only its parameters and locals)"),
            )
        } else {
            self.err(
                at,
                format!("unknown name `{n}` (not a need, mem slot or binding of this kind)"),
            )
        }
    }

    /// Emit `cond`; on false, jump to `on_false`. `top` is true while the
    /// condition is a top-level conjunct, where `as v` bindings are allowed.
    fn cond(&mut self, c: &Cond, on_false: Label, top: bool) -> Result<()> {
        match c {
            Cond::Expr(e) => {
                self.expr(e)?;
                self.asm.jz(on_false);
            }
            Cond::Nearest { pred, r, bind, at } => {
                if !top {
                    return Err(self.err(
                        at,
                        "`nearest ... as` must be a top-level conjunct (not under `or` or `not`)",
                    ));
                }
                if self.local(bind).is_some() {
                    return Err(self.err(at, format!("`{bind}` is already bound")));
                }
                self.hides(bind, &format!("binding `{bind}`"), at)?;
                let slot = self.alloc_local(at, 2)?;
                self.pred(pred)?;
                self.expr(r)?;
                self.asm.nearest(slot).jz(on_false);
                self.locals.push(Local {
                    name: bind.clone(),
                    slot,
                    ty: Ty::Target,
                });
            }
            Cond::Sniff { ch, r, bind, at } => {
                if !top {
                    return Err(self.err(
                        at,
                        "`sniff ... as` must be a top-level conjunct (not under `or` or `not`)",
                    ));
                }
                if self.local(bind).is_some() {
                    return Err(self.err(at, format!("`{bind}` is already bound")));
                }
                self.hides(bind, &format!("binding `{bind}`"), at)?;
                let slot = self.alloc_local(at, 2)?;
                let c = self.scent(ch, at)?;
                self.push_int(i32::from(c))?;
                self.expr(r)?;
                self.asm.sniff(slot).jz(on_false);
                self.locals.push(Local {
                    name: bind.clone(),
                    slot,
                    ty: Ty::Target,
                });
            }
            Cond::And(a, b) => {
                self.cond(a, on_false, top)?;
                self.cond(b, on_false, top)?;
            }
            Cond::Or(a, b) => {
                let yes = self.asm.label();
                let no = self.asm.label();
                self.cond(a, no, false)?;
                self.asm.jmp(yes).bind(no);
                self.cond(b, on_false, false)?;
                self.asm.bind(yes);
            }
            Cond::Not(inner) => {
                let yes = self.asm.label();
                self.cond(inner, yes, false)?;
                self.asm.jmp(on_false).bind(yes);
            }
        }
        Ok(())
    }

    fn pred(&mut self, p: &Pred) -> Result<()> {
        let v = match p {
            Pred::Free => pred::FREE,
            Pred::Bare => pred::BARE,
            Pred::Ground(g) => pred::ground(*g as u8),
            Pred::Feature(f) => pred::feature(*f as u8),
            Pred::Kind(name, only, at) => {
                if let Some(l) = self.local(name) {
                    if l.ty != Ty::Pred {
                        return Err(self.err(at, format!("`{name}` is not a pred")));
                    }
                    if *only {
                        return Err(self.err(
                            at,
                            format!("`only` applies to a kind, not the parameter `{name}`"),
                        ));
                    }
                    let slot = l.slot;
                    self.asm.load(slot);
                    return Ok(());
                }
                if let Some(id) = self.kind_id(name) {
                    i32::from(id) + if *only { pred::ONLY } else { 0 }
                } else if self.item_named(name).is_some() {
                    return Err(self.err(
                        at,
                        format!("`{name}` is a trait: no actor is one; match a tag instead"),
                    ));
                } else if let Some(bit) = self.tags.iter().position(|t| t == name) {
                    if *only {
                        return Err(self.err(
                            at,
                            format!("`only` applies to a kind, not the tag `{name}`"),
                        ));
                    }
                    pred::TAG_BASE + i32::try_from(bit).expect("at most MAX_TAGS tags")
                } else {
                    return Err(self.err(at, format!("unknown kind or tag `{name}`")));
                }
            }
            Pred::KindLook(name, look, only, at) => match self.kind_id(name) {
                Some(id) => pred::kind_look(id, *look) + if *only { pred::ONLY } else { 0 },
                None => {
                    return Err(self.err(at, format!("`{name}:{look}`: `{name}` is not a kind")));
                }
            },
        };
        self.push_int(v)?;
        Ok(())
    }

    /// Emit a target: `dx dy` on the stack.
    fn target(&mut self, t: &Target) -> Result<()> {
        match t {
            Target::Here => {
                self.asm.push(0).push(0);
            }
            Target::Attacker => {
                self.asm.sense(Sense::HurtDir).op(OpCode::DirOf);
            }
            Target::Heading(h) => {
                self.expr(h)?;
                self.asm.op(OpCode::DirOf);
            }
            Target::Dir(dx, dy) => {
                self.asm.push(*dx).push(*dy);
            }
            Target::Named(name, at) => {
                let l = self
                    .local(name)
                    .ok_or_else(|| self.unknown_name(at, name))?;
                if l.ty != Ty::Target {
                    return Err(self.err(at, format!("`{name}` is not a target")));
                }
                let slot = l.slot;
                self.asm.load(slot).load(slot + 1);
            }
            Target::Toward(inner) | Target::Away(inner) => {
                // (sign dx, sign dy), negated for `away`.
                let here = self.here.clone();
                let tmp = self.alloc_local(&here, 2)?;
                self.target(inner)?;
                self.asm.store(tmp + 1).store(tmp);
                self.asm.load(tmp).op(OpCode::Sign);
                if matches!(t, Target::Away(_)) {
                    self.asm.op(OpCode::Neg);
                }
                self.asm.load(tmp + 1).op(OpCode::Sign);
                if matches!(t, Target::Away(_)) {
                    self.asm.op(OpCode::Neg);
                }
                self.next_local = tmp;
            }
            Target::At(x, y) => {
                self.expr(x)?;
                self.asm.sense(Sense::X).op(OpCode::Sub);
                self.expr(y)?;
                self.asm.sense(Sense::Y).op(OpCode::Sub);
            }
            Target::RandomFree => {
                // A free neighbour chosen by the ring's rotated start, whatever
                // the sight; (0, 0) when there is none (the move is then BLOCKED).
                let here = self.here.clone();
                let tmp = self.alloc_local(&here, 2)?;
                self.asm.push(0).store(tmp).push(0).store(tmp + 1);
                self.asm.random_free(tmp).op(OpCode::Pop);
                self.asm.load(tmp).load(tmp + 1);
                self.next_local = tmp;
            }
        }
        Ok(())
    }

    fn expr(&mut self, e: &Expr) -> Result<()> {
        match e {
            Expr::Int(v) => self.push_int(*v)?,
            Expr::Sense(s) => {
                self.asm.sense(*s);
            }
            Expr::Name(n, at) => {
                if let Some(l) = self.local(n) {
                    match l.ty {
                        Ty::Int => {
                            let slot = l.slot;
                            self.asm.load(slot);
                        }
                        Ty::Target => {
                            return Err(self.err(
                                at,
                                format!("`{n}` is a target: use `{n}.dx`, `{n}.dy` or `dist({n})`"),
                            ));
                        }
                        Ty::Pred => {
                            return Err(self.err(
                                at,
                                format!(
                                    "`{n}` is a predicate: use it in count, nearest, is or for each"
                                ),
                            ));
                        }
                    }
                } else if let Some(i) = self.need_slot(n) {
                    self.asm.need(i);
                } else if let Some(i) = self.mem_slot(n) {
                    self.asm.mem(i);
                } else if let Some(v) = self.param(n).or_else(|| self.const_value(n)) {
                    self.push_int(v)?;
                } else {
                    return Err(self.unknown_name(at, n));
                }
            }
            Expr::Field(n, field, at) => {
                let l = self.local(n).ok_or_else(|| self.unknown_name(at, n))?;
                if l.ty != Ty::Target {
                    return Err(self.err(at, format!("`{n}` is not a target")));
                }
                let slot = l.slot + field;
                self.asm.load(slot);
            }
            Expr::Bin(op, a, b) => {
                self.expr(a)?;
                self.expr(b)?;
                self.asm.op(*op);
            }
            Expr::Neg(a) => {
                self.expr(a)?;
                self.asm.op(OpCode::Neg);
            }
            Expr::Rand(a) => {
                self.expr(a)?;
                self.asm.op(OpCode::Rand);
            }
            Expr::Chance(a) => {
                self.expr(a)?;
                self.asm.op(OpCode::Chance);
            }
            Expr::Count(p, r) => {
                self.pred(p)?;
                self.expr(r)?;
                self.asm.op(OpCode::Count);
            }
            Expr::Dist(t) => {
                self.target(t)?;
                self.asm.op(OpCode::Dist);
            }
            Expr::FreeAt(t) => {
                self.target(t)?;
                self.asm.op(OpCode::FreeAt);
            }
            Expr::IsAt(t, p) => {
                self.target(t)?;
                self.pred(p)?;
                self.asm.op(OpCode::IsAt);
            }
            Expr::LookOf(t) => {
                self.target(t)?;
                self.asm.op(OpCode::LookAt);
            }
            Expr::Scent(ch, t, at) => {
                let c = self.scent(ch, at)?;
                match t {
                    Some(t) => self.target(t)?,
                    None => {
                        self.asm.push(0).push(0);
                    }
                }
                self.asm.scent_at(c);
            }
            Expr::SignalOf(t, _) => {
                self.target(t)?;
                self.asm.op(OpCode::SignalAt);
            }
            Expr::Fn(op, args) => {
                for a in args {
                    self.expr(a)?;
                }
                self.asm.op(*op);
            }
            Expr::Call { name, args, at } => {
                let returns = self.call(name, args, at)?;
                if !returns {
                    return Err(self.err(
                        at,
                        format!("sub `{name}` returns nothing; it cannot be used in an expression"),
                    ));
                }
            }
        }
        Ok(())
    }

    /// Emit a call; returns whether the sub leaves a value on the stack.
    fn call(&mut self, name: &str, args: &[Arg], at: &Pos) -> Result<bool> {
        let (idx, sub): (u16, &'a SubAst) = if let Some(&(_, idx)) =
            self.members_here.iter().find(|(n, _)| n == name)
        {
            // A member sub: the owner must define it (it may be overridden).
            if let Some(owner) = self.owner
                && !self.insts[owner].members.iter().any(|(n, ..)| n == name)
            {
                return Err(self.err(
                        at,
                        format!(
                            "`{}` calls `{name}`, which it does not define (a trait or parent kind sees only its own subs)",
                            self.inst_name(owner)
                        ),
                    ));
            }
            let sub = self.callee(name).expect("a member of the current tables");
            (idx, sub)
        } else {
            let subs = self.subs;
            let i = subs
                .iter()
                .position(|s| s.name == name)
                .ok_or_else(|| self.err(at, format!("unknown sub `{name}`")))?;
            (u16::try_from(i).expect("checked in generate"), &subs[i])
        };
        // The action would survive a false condition (docs/RULES.md §4).
        if self.in_when && self.call_acts(name, 0) {
            return Err(self.err(
                at,
                format!("`{name}` may act or `next`: a sub called in a `when` condition must not"),
            ));
        }
        if args.len() != sub.params.len() {
            return Err(self.err(
                at,
                format!(
                    "sub `{name}` takes {} arguments, {} given",
                    sub.params.len(),
                    args.len()
                ),
            ));
        }
        let mut width = 0u8;
        for (arg, (pname, ty)) in args.iter().zip(&sub.params) {
            width += ty.width();
            match (ty, arg) {
                (Ty::Int, Arg::Expr(e)) => self.expr(e)?,
                (Ty::Int, Arg::Name(n, p)) => self.expr(&Expr::Name(n.clone(), p.clone()))?,
                (Ty::Target, Arg::Target(t)) => self.target(t)?,
                (Ty::Target, Arg::Name(n, p)) => {
                    self.target(&Target::Named(n.clone(), p.clone()))?;
                }
                (Ty::Pred, Arg::Name(n, p) | Arg::Expr(Expr::Name(n, p))) => {
                    // `water` is the ground, unless a local shadows it.
                    let pr = match pred_word(n) {
                        Some(pr) if self.local(n).is_none() => pr,
                        _ => Pred::Kind(n.clone(), false, p.clone()),
                    };
                    self.pred(&pr)?;
                }
                (Ty::Pred, Arg::Pred(pr)) => self.pred(pr)?,
                (ty, _) => {
                    return Err(self.err(
                        at,
                        format!(
                            "argument `{pname}` of `{name}` must be {}",
                            match ty {
                                Ty::Int => "an integer",
                                Ty::Target => "a target",
                                Ty::Pred => {
                                    "a predicate (a kind, a tag, water, soil, rock, free or bare)"
                                }
                            }
                        ),
                    ));
                }
            }
        }
        self.asm.call(idx, width);
        if sub.returns {
            self.asm.returned();
        }
        Ok(sub.returns)
    }

    fn stmts(&mut self, body: &[Stmt]) -> Result<()> {
        // A statement that acts on every path, then another action in the
        // same list: a second action, certain; refused here rather than
        // trapped at run time.
        let mut acted: Option<Option<u32>> = None;
        // Likewise a second `next` in the list: the first one's line.
        let mut chose: Option<u32> = None;
        for s in body {
            if let (Some(line), Some(at)) = (acted, action_at(s)) {
                let earlier = line.map_or(String::new(), |l| format!(" at line {l}"));
                return Err(self.err(
                    at,
                    format!(
                        "a second action: the think already acted{earlier} (one action per think)"
                    ),
                ));
            }
            if let Stmt::Next(_, at) = s {
                if let Some(line) = chose {
                    return Err(self.err(
                        at,
                        format!("a second `next`: the think already chose a state at line {line}"),
                    ));
                }
                chose = Some(at.line);
            }
            self.stmt(s)?;
            if acted.is_none() && self.ends(s, true, 0, false) {
                acted = Some(stmt_line(s));
            }
        }
        Ok(())
    }

    fn store(&mut self, name: &str, at: &Pos) -> Result<()> {
        if let Some(l) = self.local(name) {
            if l.ty != Ty::Int {
                return Err(self.err(at, format!("`{name}` is not an integer local")));
            }
            let slot = l.slot;
            self.asm.store(slot);
        } else if let Some(i) = self.need_slot(name) {
            self.asm.set_need(i);
        } else if let Some(i) = self.mem_slot(name) {
            self.asm.set_mem(i);
        } else if self.param(name).is_some() || self.const_value(name).is_some() {
            return Err(self.err(at, format!("cannot assign to `{name}`: it is a constant")));
        } else {
            return Err(self.err(
                at,
                format!("cannot assign to `{name}`: not a local, need or mem slot here"),
            ));
        }
        Ok(())
    }

    /// Run `f` with a scope: locals and slots allocated inside are released.
    fn scoped(&mut self, f: impl FnOnce(&mut Self) -> Result<()>) -> Result<()> {
        let saved = (self.locals.len(), self.next_local);
        let r = f(self);
        self.locals.truncate(saved.0);
        self.next_local = saved.1;
        r
    }

    fn stmt(&mut self, s: &Stmt) -> Result<()> {
        match s {
            Stmt::Set { name, at, value } => {
                self.expr(value)?;
                self.store(name, at)?;
            }
            Stmt::Assign {
                name,
                at,
                op,
                value,
            } => {
                self.expr(&Expr::Name(name.clone(), at.clone()))?;
                self.expr(value)?;
                self.asm.op(*op);
                self.store(name, at)?;
            }
            Stmt::Let { name, at, value } => {
                if self.local(name).is_some() {
                    return Err(self.err(at, format!("`{name}` is already bound")));
                }
                self.hides(name, &format!("local `{name}`"), at)?;
                self.expr(value)?;
                let slot = self.alloc_local(at, 1)?;
                self.asm.store(slot);
                self.locals.push(Local {
                    name: name.clone(),
                    slot,
                    ty: Ty::Int,
                });
            }
            Stmt::If { cond, then, els } => {
                let no = self.asm.label();
                let end = self.asm.label();
                self.scoped(|g| {
                    g.cond(cond, no, true)?;
                    g.stmts(then)
                })?;
                if els.is_empty() {
                    self.asm.bind(no);
                } else {
                    self.asm.jmp(end).bind(no);
                    self.scoped(|g| g.stmts(els))?;
                    self.asm.bind(end);
                }
            }
            Stmt::While { cond, body } => {
                let top = self.asm.label();
                let end = self.asm.label();
                self.asm.bind(top);
                self.scoped(|g| {
                    g.cond(cond, end, true)?;
                    g.stmts(body)
                })?;
                self.asm.jmp(top).bind(end);
            }
            Stmt::Repeat { count, body } => {
                let top = self.asm.label();
                let end = self.asm.label();
                self.scoped(|g| {
                    let here = g.here.clone();
                    let n = g.alloc_local(&here, 1)?;
                    g.expr(count)?;
                    g.asm.store(n).bind(top);
                    g.asm.load(n).push(0).op(OpCode::Gt).jz(end);
                    g.stmts(body)?;
                    g.asm.load(n).push(1).op(OpCode::Sub).store(n).jmp(top);
                    Ok(())
                })?;
                self.asm.bind(end);
            }
            Stmt::Choose(arms) => self.choose(arms)?,
            Stmt::Call { name, args, at } => {
                if self.call(name, args, at)? {
                    self.asm.op(OpCode::Pop);
                }
            }
            Stmt::Return { value, at } => {
                let Some(sub) = self.sub else {
                    return Err(self.err(at, "`return` outside a sub"));
                };
                match (value, sub.returns) {
                    (Some(e), true) => {
                        self.expr(e)?;
                        self.asm.ret(true);
                    }
                    (None, false) => {
                        self.asm.ret(false);
                    }
                    (None, true) => {
                        return Err(self.err(at, "this sub returns a value: `return <expr>`"));
                    }
                    (Some(_), false) => unreachable!("returns_value saw this return"),
                }
            }
            Stmt::Idle(_) => {
                self.asm.act(Action::Idle);
            }
            Stmt::Die(_) => {
                self.asm.act(Action::Die);
            }
            Stmt::Become { kind, at } => {
                let id = self.concrete(kind, at)?;
                self.push_int(i32::from(id))?;
                self.asm.act(Action::Become);
            }
            Stmt::Spawn {
                kind,
                at,
                pos,
                with,
            } => {
                let id = self.concrete(kind, pos)?;
                self.push_int(i32::from(id))?;
                self.target(at)?;
                if !with.is_empty() {
                    // The kind's first two slots, in its layout order.
                    let slots = self
                        .withs
                        .iter()
                        .find(|(k, _)| k == kind)
                        .map(|(_, n)| n.clone())
                        .expect("collected before codegen");
                    for i in 0..2 {
                        match slots
                            .get(i)
                            .and_then(|(n, _)| with.iter().find(|w| w.0 == *n))
                        {
                            Some((_, e, _)) => self.expr(e)?,
                            None => self.push_int(0)?,
                        }
                    }
                    self.asm.op(OpCode::SpawnWith);
                }
                self.asm.act(Action::Spawn);
            }
            Stmt::Transfer {
                give,
                target,
                need,
                amount,
                at,
            } => {
                let verb = if *give { "give" } else { "take" };
                if self.cur.is_none() {
                    return Err(
                        self.err(at, format!("`{verb}` inside a sub: needs belong to a kind"))
                    );
                }
                let slot = self.need_slot(need).ok_or_else(|| {
                    self.err(at, format!("`{verb}`: this kind has no need `{need}`"))
                })?;
                self.target(target)?;
                self.push_int(i32::from(slot))?;
                self.expr(amount)?;
                self.asm
                    .act(if *give { Action::Give } else { Action::Take });
            }
            Stmt::Move(t, _) => {
                self.target(t)?;
                self.asm.act(Action::Move);
            }
            Stmt::Drink(t, _) => {
                self.target(t)?;
                self.asm.act(Action::Drink);
            }
            Stmt::Eat(t, _) => {
                self.target(t)?;
                self.asm.act(Action::Eat);
            }
            Stmt::Hit(t, _) => {
                self.target(t)?;
                self.asm.act(Action::Hit);
            }
            Stmt::Graze(t, _) => {
                self.target(t)?;
                self.asm.act(Action::Graze);
            }
            Stmt::Look(e) => {
                self.expr(e)?;
                self.asm.op(OpCode::SetLook);
            }
            Stmt::Signal(e) => {
                self.expr(e)?;
                self.asm.op(OpCode::SetSignal);
            }
            Stmt::Mark(ch, e, at) => {
                let c = self.scent(ch, at)?;
                self.expr(e)?;
                self.asm.mark(c);
            }
            Stmt::Next(name, at) => {
                let (Some(cur), Some(owner)) = (self.cur, self.owner) else {
                    return Err(self.err(at, "`next` inside a sub: states belong to a kind"));
                };
                if !self.insts[owner].states.iter().any(|st| st == name) {
                    let o = &self.items[self.insts[owner].item];
                    return Err(self.err(
                        at,
                        format!(
                            "{} `{}` has no state `{name}`",
                            if o.is_trait { "trait" } else { "kind" },
                            o.name
                        ),
                    ));
                }
                let s = self.insts[cur]
                    .states
                    .iter()
                    .position(|st| st == name)
                    .expect("the owner's states are the kind's");
                self.asm
                    .next(u8::try_from(s).expect("at most MAX_STATES states"));
            }
            Stmt::ForEach {
                pred,
                r,
                bind,
                body,
                at,
            } => {
                if self.local(bind).is_some() {
                    return Err(self.err(at, format!("`{bind}` is already bound")));
                }
                self.hides(bind, &format!("binding `{bind}`"), at)?;
                self.scoped(|g| {
                    let base = g.alloc_local(at, FOR_EACH_LOCALS.into())?;
                    g.pred(pred)?;
                    g.asm.store(base + 3);
                    g.expr(r)?;
                    g.asm.store(base + 4).push(0).store(base + 2);
                    let top = g.asm.label();
                    let end = g.asm.label();
                    g.asm.bind(top).for_each(base).jz(end);
                    g.locals.push(Local {
                        name: bind.clone(),
                        slot: base,
                        ty: Ty::Target,
                    });
                    g.scoped(|g| g.stmts(body))?;
                    g.asm.jmp(top).bind(end);
                    Ok(())
                })?;
            }
        }
        Ok(())
    }

    /// `choose { w1: b1 ... wn: bn }`: weights into locals, one draw in
    /// `0..total`, the first arm whose cumulative weight exceeds it runs.
    /// A total of zero runs nothing.
    fn choose(&mut self, arms: &[(Expr, Vec<Stmt>)]) -> Result<()> {
        let n = arms.len();
        let here = self.here.clone();
        self.scoped(|g| {
            let base = g.alloc_local(&here, n + 1)?;
            // alloc_local has already held n to 15.
            let n = u8::try_from(n).expect("at most FRAME_LOCALS arms");
            let draw = base + n;
            // Each weight is capped at i32::MAX / n, so the total cannot wrap.
            let cap = i32::MAX / i32::from(n);
            g.asm.push(0);
            for (i, (w, _)) in (0u8..).zip(arms) {
                g.expr(w)?;
                g.asm.push(0).op(OpCode::Max);
                g.push_int(cap)?;
                g.asm.op(OpCode::Min).store(base + i);
                g.asm.load(base + i).op(OpCode::Add);
            }
            g.asm.op(OpCode::Rand).store(draw);
            let end = g.asm.label();
            for (i, (_, body)) in (0u8..).zip(arms) {
                let skip = g.asm.label();
                g.asm.load(draw).load(base + i).op(OpCode::Lt).jz(skip);
                g.scoped(|g| g.stmts(body))?;
                g.asm.jmp(end).bind(skip);
                if i + 1 < n {
                    g.asm.load(draw).load(base + i).op(OpCode::Sub).store(draw);
                }
            }
            g.asm.bind(end);
            Ok(())
        })
    }
}

#[cfg(test)]
mod props;
#[cfg(test)]
mod tests;
