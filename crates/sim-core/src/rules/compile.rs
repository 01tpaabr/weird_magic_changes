//! The rules compiler: text -> [`Kinds`] (`docs/ACTORS.md` §5).
//!
//! Three small passes over one file: a lexer (tokens with line and column),
//! a recursive-descent parser (an AST per kind and per sub), and a code
//! generator that drives [`Asm`]. Kinds are numbered in declaration order,
//! files in sorted-name order, never directory order, so two processes agree
//! on every kind id. Every error carries `file:line:col`; the sim never runs
//! a program that did not compile.
//!
//! Subs are file-scope and shared by every kind, so inside a sub a name is a
//! parameter or a local, never a need or a mem slot. A sub that `return`s a
//! value anywhere is a function (usable in expressions), otherwise a
//! procedure (a statement). Targets are `(dx, dy)` pairs: two stack values
//! in flight, two locals at rest.

use std::fmt;

use super::asm::{Asm, Label};
use super::vm::{Action, FOR_EACH_LOCALS, FRAME_LOCALS, OpCode, Sense, pred, result};
use super::{DEFAULT_COLOR, KindDef, Kinds, NeedDef, PLACE_ONE};
use crate::actors::{MEM_SLOTS, NEED_SLOTS};
use crate::stage::{Feature, Ground, SCENT_CHANNELS};
use crate::time::{days, hours, minutes};

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
    let mut items = Items::default();
    for (name, text) in files {
        let tokens = Lexer::new(name, text).lex()?;
        let mut p = Parser {
            file: name,
            tokens,
            at: 0,
        };
        p.file(&mut items)?;
    }
    Gen::new(&items).generate()
}

/// Compile every `*.rules` file in `dir`, in sorted file-name order.
pub fn compile_dir(
    dir: &std::path::Path,
) -> std::result::Result<Kinds, Box<dyn std::error::Error>> {
    let mut paths: Vec<_> = std::fs::read_dir(dir)?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "rules"))
        .collect();
    paths.sort();
    let mut texts = Vec::new();
    for p in &paths {
        let name = p
            .file_name()
            .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
        texts.push((name, std::fs::read_to_string(p)?));
    }
    let files: Vec<(&str, &str)> = texts
        .iter()
        .map(|(n, t)| (n.as_str(), t.as_str()))
        .collect();
    Ok(compile_files(&files)?)
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
    src: &'a [u8],
    at: usize,
    line: u32,
    col: u32,
}

impl<'a> Lexer<'a> {
    fn new(file: &'a str, text: &'a str) -> Self {
        Self {
            file,
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

    fn peek(&self, k: usize) -> u8 {
        self.src.get(self.at + k).copied().unwrap_or(0)
    }

    fn bump(&mut self) -> u8 {
        let b = self.peek(0);
        self.at += 1;
        if b == b'\n' {
            self.line += 1;
            self.col = 1;
        } else {
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
                        while !matches!(self.peek(0), b'\n' | 0) {
                            self.bump();
                        }
                    }
                    _ => break,
                }
            }
            let (line, col) = (self.line, self.col);
            let b = self.peek(0);
            let tok = if b == 0 {
                Tok::Eof
            } else if b.is_ascii_digit() {
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
                let ticks = |t: u64| i32::try_from(t).ok();
                match unit.as_str() {
                    "" => Tok::Int(v as i32),
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
                        0 | b'\n' => return Err(self.err("unterminated string")),
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
                    .ok_or_else(|| self.err(format!("unexpected character `{}`", char::from(b))))?;
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

#[derive(Debug, Clone)]
struct KindAst {
    at: Pos,
    name: String,
    glyph: u8,
    tags: Vec<String>,
    cadence_shift: u8,
    sight: u8,
    fuel: u32,
    food: i32,
    bite: u8,
    /// Worldgen share, out of `PLACE_ONE`.
    place: u32,
    /// `0xRRGGBB`, opaque to the sim: the palette draws the glyph in it.
    color: u32,
    /// Ground cover (`cover` declaration).
    cover: bool,
    needs: Vec<NeedDef>,
    mems: Vec<String>,
    /// Reflexes: scanned first on every think, whatever the state.
    rules: Vec<Rule>,
    states: Vec<StateAst>,
}

/// Everything the files declare, in file order then declaration order.
#[derive(Debug, Default)]
struct Items {
    kinds: Vec<KindAst>,
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
    rules: Vec<Rule>,
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
    Idle,
    Die,
    Become {
        kind: String,
        at: Pos,
    },
    Spawn {
        kind: String,
        at: Target,
        pos: Pos,
        /// `with (a, b)`: the child's first two `mem` values.
        with: Option<(Expr, Expr)>,
    },
    /// `take t NEED amount` / `give t NEED amount`.
    Transfer {
        give: bool,
        target: Target,
        need: String,
        amount: Expr,
        at: Pos,
    },
    Move(Target),
    Drink(Target),
    Eat(Target),
    Hit(Target),
    Graze(Target),
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
    Kind(String, Pos),
    /// `kind:look`: that kind showing that look byte.
    KindLook(String, u8, Pos),
    Ground(Ground),
    Feature(Feature),
    Free,
    Bare,
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
    SignalOf(Target),
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
}

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
        "ground" => Sense::Ground,
        "feature" => Sense::Feature,
        "taken" => Sense::Taken,
        "trapped" => Sense::Trapped,
        _ => return None,
    })
}

// `water`, `soil`, `rock` and `free` are contextual: predicates after
// `count`/`nearest`/`is`/`random`, plain names elsewhere (so a kind may
// declare `need water`). `food` likewise: a declaration where a declaration
// starts, a need name everywhere else (`need food`, `food < 12h`).
const KEYWORDS: &[&str] = &[
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

    fn int(&mut self, what: &str) -> Result<i32> {
        match self.bump() {
            Tok::Int(v) => Ok(v),
            _ => {
                self.at -= 1;
                Err(self.err(format!("expected {what}, found {}", self.describe())))
            }
        }
    }

    fn int_or_time(&mut self, what: &str) -> Result<i32> {
        match self.bump() {
            Tok::Int(v) | Tok::Time(v) => Ok(v),
            _ => {
                self.at -= 1;
                Err(self.err(format!("expected {what}, found {}", self.describe())))
            }
        }
    }

    fn file(&mut self, items: &mut Items) -> Result<()> {
        while *self.peek() != Tok::Eof {
            if self.is_kw("kind") {
                items.kinds.push(self.kind()?);
            } else if self.is_kw("sub") {
                items.subs.push(self.sub()?);
            } else if self.eat_kw("const") {
                let (name, at) = self.ident("constant name")?;
                self.expect_sym("=")?;
                let value = self.expr()?;
                items.consts.push(ConstAst { at, name, value });
            } else {
                return Err(self.err(format!(
                    "expected `kind`, `sub` or `const`, found {}",
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

    fn kind(&mut self) -> Result<KindAst> {
        self.expect_kw("kind")?;
        let (name, at) = self.ident("kind name")?;
        self.expect_sym("{")?;
        let mut k = KindAst {
            at,
            name,
            glyph: b'?',
            tags: Vec::new(),
            cadence_shift: 3,
            sight: 4,
            fuel: 512,
            food: 0,
            bite: 1,
            place: 0,
            color: DEFAULT_COLOR,
            cover: false,
            needs: Vec::new(),
            mems: Vec::new(),
            rules: Vec::new(),
            states: Vec::new(),
        };
        // Declarations, then rules; a declaration after a rule is an error
        // so a file reads top-down.
        while !self.is_sym("}") {
            let p = self.pos();
            if self.eat_kw("glyph") {
                match self.bump() {
                    Tok::Str(s) if s.len() == 1 && s.as_bytes()[0].is_ascii_graphic() => {
                        k.glyph = s.as_bytes()[0];
                    }
                    _ => {
                        return Err(self.err_at(&p, "glyph takes one printable ASCII character"));
                    }
                }
            } else if self.eat_kw("tags") {
                while let Tok::Name(n) = self.peek().clone() {
                    if is_reserved(&n) {
                        break;
                    }
                    self.bump();
                    k.tags.push(n);
                }
            } else if self.eat_kw("cadence") {
                let c = self.int("a cadence")?;
                if c < 1 || !(c as u32).is_power_of_two() {
                    return Err(self.err_at(&p, "cadence must be a power of two (1, 2, 4, ...)"));
                }
                k.cadence_shift = c.trailing_zeros() as u8;
            } else if self.eat_kw("sight") {
                let s = self.int("a sight radius")?;
                if !(0..=16).contains(&s) {
                    return Err(self.err_at(&p, "sight is 0 to 16 cells"));
                }
                k.sight = s as u8;
            } else if self.eat_kw("fuel") {
                let f = self.int("a fuel budget")?;
                if !(1..=4096).contains(&f) {
                    return Err(self.err_at(&p, "fuel is 1 to 4096 ops per think"));
                }
                k.fuel = f as u32;
            } else if self.eat_kw("food") {
                k.food = self.int_or_time("a food value")?;
            } else if self.eat_kw("cover") {
                k.cover = true;
            } else if self.eat_kw("color") {
                // `color "#rrggbb"`
                let c = match self.bump() {
                    Tok::Str(s) if s.len() == 7 && s.starts_with('#') => {
                        u32::from_str_radix(&s[1..], 16).ok()
                    }
                    _ => None,
                };
                k.color = c.ok_or_else(|| self.err_at(&p, "color takes \"#rrggbb\""))?;
            } else if self.eat_kw("place") {
                // `place N / D`: this share of walkable cells starts as this kind.
                let n = self.int("a numerator")?;
                self.expect_sym("/")?;
                let d = self.int("a denominator")?;
                if n < 0 || d < 1 || n > d {
                    return Err(self.err_at(&p, "place is N / D with 0 <= N <= D"));
                }
                k.place = (u64::from(n as u32) * u64::from(PLACE_ONE) / u64::from(d as u32)) as u32;
            } else if self.eat_kw("bite") {
                let b = self.int("a bite")?;
                if !(0..=255).contains(&b) {
                    return Err(self.err_at(&p, "bite is 0 to 255"));
                }
                k.bite = b as u8;
            } else if self.eat_kw("need") {
                let (name, ..) = self.ident("need name")?;
                self.expect_kw("max")?;
                let max = self.int_or_time("the need's maximum")?;
                if max < 1 {
                    return Err(self.err_at(&p, "a need's max is at least 1"));
                }
                let mut decays = true;
                if self.eat_kw("decay") {
                    match self.int("0 (points) or 1 (per tick)")? {
                        0 => decays = false,
                        1 => decays = true,
                        _ => return Err(self.err_at(&p, "decay is 0 (points) or 1 (per tick)")),
                    }
                }
                let vital = self.eat_kw("vital");
                if k.needs.iter().any(|n| n.name == name) {
                    return Err(self.err_at(&p, format!("need `{name}` declared twice")));
                }
                if k.needs.len() == NEED_SLOTS {
                    return Err(self.err_at(&p, format!("at most {NEED_SLOTS} needs per kind")));
                }
                k.needs.push(NeedDef {
                    name,
                    max,
                    decays,
                    vital,
                });
            } else if self.eat_kw("mem") {
                loop {
                    let (name, ..) = self.ident("memory slot name")?;
                    if k.mems.contains(&name) || k.needs.iter().any(|n| n.name == name) {
                        return Err(self.err_at(&p, format!("`{name}` declared twice")));
                    }
                    if k.mems.len() == MEM_SLOTS {
                        return Err(
                            self.err_at(&p, format!("at most {MEM_SLOTS} mem slots per kind"))
                        );
                    }
                    k.mems.push(name);
                    if !self.eat_sym(",") {
                        break;
                    }
                }
            } else if self.is_kw("when") || self.is_kw("state") {
                break;
            } else {
                return Err(self.err(format!(
                    "expected a declaration, `when` or `state`, found {}",
                    self.describe()
                )));
            }
        }
        k.rules = self.rules()?;
        while self.eat_kw("state") {
            let (name, at) = self.ident("state name")?;
            if k.states.iter().any(|s| s.name == name) {
                return Err(self.err_at(&at, format!("state `{name}` declared twice")));
            }
            if k.states.len() == 64 {
                return Err(self.err_at(&at, "at most 64 states per kind"));
            }
            self.expect_sym("{")?;
            let rules = self.rules()?;
            if !self.is_sym("}") {
                return Err(self.err(format!(
                    "expected `when` or `}}` in state `{name}`, found {}",
                    self.describe()
                )));
            }
            self.expect_sym("}")?;
            k.states.push(StateAst { at, name, rules });
        }
        if !self.is_sym("}") {
            return Err(self.err(format!(
                "expected `when`, `state` or `}}`, found {}",
                self.describe()
            )));
        }
        self.expect_sym("}")?;
        Ok(k)
    }

    /// `when cond => body`, as many as there are.
    fn rules(&mut self) -> Result<Vec<Rule>> {
        let mut rules = Vec::new();
        while self.eat_kw("when") {
            let cond = self.cond()?;
            self.expect_sym("=>")?;
            let body = self.body()?;
            rules.push(Rule { cond, body });
        }
        Ok(rules)
    }

    fn body(&mut self) -> Result<Vec<Stmt>> {
        if self.is_sym("{") {
            self.block()
        } else {
            Ok(vec![self.stmt()?])
        }
    }

    fn block(&mut self) -> Result<Vec<Stmt>> {
        self.expect_sym("{")?;
        let mut stmts = Vec::new();
        while !self.is_sym("}") {
            if *self.peek() == Tok::Eof {
                return Err(self.err("unclosed block"));
            }
            stmts.push(self.stmt()?);
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
                    vec![self.stmt()?]
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
            let value = if self.is_sym("}") || self.is_sym(";") {
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
            return Ok(Stmt::Idle);
        }
        if self.eat_kw("die") {
            return Ok(Stmt::Die);
        }
        if self.eat_kw("become") {
            let (kind, at) = self.ident("kind name")?;
            return Ok(Stmt::Become { kind, at });
        }
        if self.eat_kw("spawn") {
            let (kind, pos) = self.ident("kind name")?;
            self.expect_kw("at")?;
            let at = self.target()?;
            let with = if self.eat_kw("with") {
                self.expect_sym("(")?;
                let a = self.expr()?;
                self.expect_sym(",")?;
                let b = self.expr()?;
                self.expect_sym(")")?;
                Some((a, b))
            } else {
                None
            };
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
            return Ok(Stmt::Move(self.target()?));
        }
        if self.eat_kw("drink") {
            return Ok(Stmt::Drink(self.target()?));
        }
        if self.eat_kw("eat") {
            return Ok(Stmt::Eat(self.target()?));
        }
        if self.eat_kw("hit") {
            return Ok(Stmt::Hit(self.target()?));
        }
        if self.eat_kw("graze") {
            return Ok(Stmt::Graze(self.target()?));
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
                } else if matches!(self.peek(), Tok::Name(n) if !is_reserved(n))
                    && matches!(self.peek2(), Tok::Sym(":"))
                {
                    Arg::Pred(self.pred()?)
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
            return Ok(Target::Toward(Box::new(self.target()?)));
        }
        if self.eat_kw("away") {
            return Ok(Target::Away(Box::new(self.target()?)));
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
        let mut c = self.and_cond()?;
        while self.eat_kw("or") {
            let r = self.and_cond()?;
            c = Cond::Or(Box::new(c), Box::new(r));
        }
        Ok(c)
    }

    fn and_cond(&mut self) -> Result<Cond> {
        let mut c = self.not_cond()?;
        while self.eat_kw("and") {
            let r = self.not_cond()?;
            c = Cond::And(Box::new(c), Box::new(r));
        }
        Ok(c)
    }

    fn not_cond(&mut self) -> Result<Cond> {
        if self.eat_kw("not") {
            return Ok(Cond::Not(Box::new(self.not_cond()?)));
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
            // parentheses handle the rest.
            let save = self.at;
            self.bump();
            if let Ok(c) = self.cond()
                && self.eat_sym(")")
                && !self.starts_binop()
            {
                return Ok(c);
            }
            self.at = save;
        }
        Ok(Cond::Expr(self.expr()?))
    }

    fn starts_binop(&self) -> bool {
        matches!(self.peek(), Tok::Sym(s) if ["+", "-", "*", "/", "%", "<", "<=", "==", "!=", ">=", ">"].contains(s))
    }

    fn pred(&mut self) -> Result<Pred> {
        let at = self.pos();
        if self.eat_kw("free") {
            return Ok(Pred::Free);
        }
        if self.eat_kw("bare") {
            return Ok(Pred::Bare);
        }
        if self.eat_kw("water") {
            return Ok(Pred::Ground(Ground::Water));
        }
        if self.eat_kw("soil") {
            return Ok(Pred::Ground(Ground::Soil));
        }
        if self.eat_kw("rock") {
            return Ok(Pred::Feature(Feature::Rock));
        }
        let (name, ..) =
            self.ident("predicate (a kind, a tag, water, soil, rock, free or bare)")?;
        if self.eat_sym(":") {
            let look = self.int("a look value (0 to 255)")?;
            let look =
                u8::try_from(look).map_err(|_| self.err_at(&at, "a look value is 0 to 255"))?;
            return Ok(Pred::KindLook(name, look, at));
        }
        Ok(Pred::Kind(name, at))
    }

    // expr := cmp
    fn expr(&mut self) -> Result<Expr> {
        let mut e = self.additive()?;
        loop {
            let op = match self.peek() {
                Tok::Sym("<") => OpCode::Lt,
                Tok::Sym("<=") => OpCode::Le,
                Tok::Sym("==") => OpCode::Eq,
                Tok::Sym("!=") => OpCode::Ne,
                Tok::Sym(">=") => OpCode::Ge,
                Tok::Sym(">") => OpCode::Gt,
                _ => return Ok(e),
            };
            self.bump();
            let r = self.additive()?;
            e = Expr::Bin(op, Box::new(e), Box::new(r));
        }
    }

    fn additive(&mut self) -> Result<Expr> {
        let mut e = self.term()?;
        loop {
            let op = match self.peek() {
                Tok::Sym("+") => OpCode::Add,
                Tok::Sym("-") => OpCode::Sub,
                _ => return Ok(e),
            };
            self.bump();
            let r = self.term()?;
            e = Expr::Bin(op, Box::new(e), Box::new(r));
        }
    }

    fn term(&mut self) -> Result<Expr> {
        let mut e = self.unary()?;
        loop {
            let op = match self.peek() {
                Tok::Sym("*") => OpCode::Mul,
                Tok::Sym("/") => OpCode::Div,
                Tok::Sym("%") => OpCode::Mod,
                _ => return Ok(e),
            };
            self.bump();
            let r = self.unary()?;
            e = Expr::Bin(op, Box::new(e), Box::new(r));
        }
    }

    fn unary(&mut self) -> Result<Expr> {
        if self.eat_sym("-") {
            return Ok(match self.unary()? {
                Expr::Int(v) => Expr::Int(v.wrapping_neg()),
                e => Expr::Neg(Box::new(e)),
            });
        }
        self.primary()
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
                        let r = self.additive()?; // a radius, never a comparison
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
                            Expr::SignalOf(t)
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
            _ => {
                self.at -= 1;
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
        _ => false,
    })
}

// ---- code generation ------------------------------------------------------------------

#[derive(Debug, Clone)]
struct Local {
    name: String,
    slot: u8,
    ty: Ty,
}

struct Gen<'a> {
    kinds: &'a [KindAst],
    subs: &'a [SubAst],
    consts: &'a [ConstAst],
    /// Folded `const` values, in declaration order.
    const_vals: Vec<(String, i32)>,
    asm: Asm,
    pool: Vec<i32>,
    /// Current kind (`None` inside a sub), the sub being compiled, locals.
    kind: Option<usize>,
    sub: Option<usize>,
    locals: Vec<Local>,
    next_local: u8,
    /// Position for errors without a better one.
    here: Pos,
    /// Tag names, in first-appearance order (kind order, then declaration
    /// order): a tag's bit is its index here.
    tags: Vec<String>,
    /// Scent channel names, in first-appearance order in the code.
    scents: Vec<String>,
}

impl<'a> Gen<'a> {
    fn new(items: &'a Items) -> Self {
        Self {
            kinds: &items.kinds,
            subs: &items.subs,
            consts: &items.consts,
            const_vals: Vec::new(),
            tags: Vec::new(),
            scents: Vec::new(),
            asm: Asm::new(),
            pool: Vec::new(),
            kind: None,
            sub: None,
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

    fn kind_id(&self, name: &str) -> Option<u16> {
        self.kinds
            .iter()
            .position(|k| k.name == name)
            .map(|i| i as u16)
    }

    fn generate(mut self) -> Result<Kinds> {
        for (i, k) in self.kinds.iter().enumerate() {
            if self.kinds[..i].iter().any(|o| o.name == k.name) {
                return Err(self.err(&k.at, format!("kind `{}` declared twice", k.name)));
            }
        }
        for (i, s) in self.subs.iter().enumerate() {
            if self.subs[..i].iter().any(|o| o.name == s.name) {
                return Err(self.err(&s.at, format!("sub `{}` declared twice", s.name)));
            }
            let width: u32 = s.params.iter().map(|(_, t)| u32::from(t.width())).sum();
            if width > FRAME_LOCALS as u32 {
                return Err(self.err(&s.at, format!("sub `{}` has too many parameters", s.name)));
            }
        }
        // Tags: global names, a bit each, never a kind's name.
        let mut placed = 0u64;
        for k in self.kinds {
            for t in &k.tags {
                if self.kind_id(t).is_some() {
                    return Err(self.err(&k.at, format!("tag `{t}` is also a kind's name")));
                }
                if !self.tags.contains(t) {
                    if self.tags.len() == 64 {
                        return Err(self.err(&k.at, "at most 64 tags in a rule set"));
                    }
                    self.tags.push(t.clone());
                }
            }
            placed += u64::from(k.place);
            if placed > u64::from(PLACE_ONE) {
                return Err(self.err(
                    &k.at,
                    "the `place` shares of all kinds add up to more than 1",
                ));
            }
        }
        // Constants: global names, folded in declaration order (a constant
        // may use the ones above it).
        for c in self.consts {
            let taken = self.const_vals.iter().any(|(n, _)| *n == c.name)
                || self.kind_id(&c.name).is_some()
                || self.tags.contains(&c.name)
                || self.subs.iter().any(|s| s.name == c.name);
            if taken {
                return Err(self.err(&c.at, format!("`{}` is already a name", c.name)));
            }
            let v = self.fold(&c.value)?;
            self.const_vals.push((c.name.clone(), v));
        }
        let mut defs = Vec::with_capacity(self.kinds.len());
        for i in 0..self.kinds.len() {
            self.kind = Some(i);
            self.sub = None;
            let entry = self.asm.here();
            let k = &self.kinds[i];
            self.here = k.at.clone();
            self.rules(&k.rules)?;
            // States: the current one's rules after the reflexes. An actor
            // starts in the first; a state out of range runs no rules.
            for (s, st) in k.states.iter().enumerate() {
                self.here = st.at.clone();
                let skip = self.asm.label();
                self.asm
                    .sense(Sense::State)
                    .push(s as i32)
                    .op(OpCode::Eq)
                    .jz(skip);
                self.rules(&st.rules)?;
                self.asm.halt().bind(skip);
            }
            self.asm.halt();
            let tags = k.tags.iter().fold(0u64, |bits, t| {
                bits | 1
                    << self
                        .tags
                        .iter()
                        .position(|x| x == t)
                        .expect("collected above")
            });
            defs.push(KindDef {
                id: i as u16,
                name: k.name.clone(),
                glyph: k.glyph,
                tags,
                cadence_shift: k.cadence_shift,
                sight: k.sight,
                fuel: k.fuel,
                food: k.food,
                bite: k.bite,
                needs: k.needs.clone(),
                mems: k.mems.clone(),
                states: k.states.len().max(1) as u8,
                entry,
                place: k.place,
                color: k.color,
                cover: k.cover,
            });
        }
        let mut sub_entries = Vec::with_capacity(self.subs.len());
        for i in 0..self.subs.len() {
            self.kind = None;
            self.sub = Some(i);
            let s = &self.subs[i];
            self.here = s.at.clone();
            sub_entries.push(self.asm.here());
            self.locals.clear();
            self.next_local = 0;
            for (name, ty) in &s.params {
                let slot = self.alloc_local(&s.at, ty.width())?;
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
        }
        let code = self.asm.finish();
        Ok(Kinds::from_parts(defs, code, self.pool, sub_entries).with_scents(self.scents))
    }

    fn rules(&mut self, rules: &[Rule]) -> Result<()> {
        for rule in rules {
            self.locals.clear();
            self.next_local = 0;
            let next = self.asm.label();
            self.cond(&rule.cond, next, true)?;
            self.stmts(&rule.body)?;
            self.asm.end_rule().bind(next);
        }
        Ok(())
    }

    /// The channel of scent `name`, numbered on first use.
    fn scent(&mut self, name: &str, at: &Pos) -> Result<u8> {
        if let Some(i) = self.scents.iter().position(|s| s == name) {
            return Ok(i as u8);
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
        Ok((self.scents.len() - 1) as u8)
    }

    fn const_value(&self, n: &str) -> Option<i32> {
        self.const_vals
            .iter()
            .find(|(c, _)| c == n)
            .map(|&(_, v)| v)
    }

    /// Fold a `const` expression: numbers, other constants, arithmetic,
    /// comparisons and the pure functions. Same results as the VM.
    fn fold(&self, e: &Expr) -> Result<i32> {
        Ok(match e {
            Expr::Int(v) => *v,
            Expr::Name(n, at) => self
                .const_value(n)
                .ok_or_else(|| self.err(at, format!("`{n}` is not a constant declared above")))?,
            Expr::Neg(a) => self.fold(a)?.wrapping_neg(),
            Expr::Bin(op, a, b) => {
                let (x, y) = (self.fold(a)?, self.fold(b)?);
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
                    _ => i32::from(x > y),
                }
            }
            Expr::Fn(op, args) => {
                let v = args
                    .iter()
                    .map(|a| self.fold(a))
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
                    _ => i32::from(v[0] as u8 as i8),
                }
            }
            _ => {
                let at = self.here.clone();
                return Err(self.err(
                    &at,
                    "a constant must be a number, other constants and arithmetic",
                ));
            }
        })
    }

    fn push_int(&mut self, v: i32) {
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
            self.asm
                .push_k(u16::try_from(idx).expect("constant pool fits u16"));
        }
    }

    fn alloc_local(&mut self, at: &Pos, n: u8) -> Result<u8> {
        let slot = self.next_local;
        if usize::from(slot) + usize::from(n) > FRAME_LOCALS {
            return Err(self.err(
                at,
                format!(
                    "too many bindings and locals in one rule or sub (max {FRAME_LOCALS} slots)"
                ),
            ));
        }
        self.next_local += n;
        Ok(slot)
    }

    fn local(&self, name: &str) -> Option<&Local> {
        self.locals.iter().rev().find(|l| l.name == name)
    }

    fn need_slot(&self, n: &str) -> Option<u8> {
        let k = &self.kinds[self.kind?];
        k.needs.iter().position(|d| d.name == n).map(|i| i as u8)
    }

    fn mem_slot(&self, n: &str) -> Option<u8> {
        let k = &self.kinds[self.kind?];
        k.mems.iter().position(|m| m == n).map(|i| i as u8)
    }

    fn unknown_name(&self, at: &Pos, n: &str) -> CompileError {
        if self.sub.is_some() {
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
                let slot = self.alloc_local(at, 2)?;
                let c = self.scent(ch, at)?;
                self.push_int(i32::from(c));
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
            Pred::Kind(name, at) => {
                if let Some(l) = self.local(name) {
                    if l.ty != Ty::Pred {
                        return Err(self.err(at, format!("`{name}` is not a pred")));
                    }
                    let slot = l.slot;
                    self.asm.load(slot);
                    return Ok(());
                }
                if let Some(id) = self.kind_id(name) {
                    i32::from(id)
                } else if let Some(bit) = self.tags.iter().position(|t| t == name) {
                    pred::TAG_BASE + bit as i32
                } else {
                    return Err(self.err(at, format!("unknown kind or tag `{name}`")));
                }
            }
            Pred::KindLook(name, look, at) => match self.kind_id(name) {
                Some(id) => pred::kind_look(id, *look),
                None => {
                    return Err(self.err(at, format!("`{name}:{look}`: `{name}` is not a kind")));
                }
            },
        };
        self.push_int(v);
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
                // A free neighbour chosen by the ring's rotated start; (0, 0)
                // when there is none (the move is then BLOCKED).
                let here = self.here.clone();
                let tmp = self.alloc_local(&here, 2)?;
                self.asm.push(0).store(tmp).push(0).store(tmp + 1);
                self.push_int(pred::FREE);
                self.asm.push(1).nearest(tmp).op(OpCode::Pop);
                self.asm.load(tmp).load(tmp + 1);
                self.next_local = tmp;
            }
        }
        Ok(())
    }

    fn expr(&mut self, e: &Expr) -> Result<()> {
        match e {
            Expr::Int(v) => self.push_int(*v),
            Expr::Sense(s) => {
                self.asm.sense(*s);
            }
            Expr::Name(n, at) => {
                if let Some(l) = self.local(n) {
                    match l.ty {
                        Ty::Int | Ty::Pred => {
                            let slot = l.slot;
                            self.asm.load(slot);
                        }
                        Ty::Target => {
                            return Err(self.err(
                                at,
                                format!("`{n}` is a target: use `{n}.dx`, `{n}.dy` or `dist({n})`"),
                            ));
                        }
                    }
                } else if let Some(i) = self.need_slot(n) {
                    self.asm.need(i);
                } else if let Some(i) = self.mem_slot(n) {
                    self.asm.mem(i);
                } else if let Some(v) = self.const_value(n) {
                    self.push_int(v);
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
            Expr::SignalOf(t) => {
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
        let idx = self
            .subs
            .iter()
            .position(|s| s.name == name)
            .ok_or_else(|| self.err(at, format!("unknown sub `{name}`")))?;
        let sub = &self.subs[idx];
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
                (Ty::Pred, Arg::Name(n, p)) => self.pred(&Pred::Kind(n.clone(), p.clone()))?,
                (Ty::Pred, Arg::Pred(pr)) => self.pred(pr)?,
                (Ty::Pred, Arg::Expr(Expr::Name(n, p))) => {
                    self.pred(&Pred::Kind(n.clone(), p.clone()))?;
                }
                (ty, _) => {
                    return Err(self.err(
                        at,
                        format!(
                            "argument `{pname}` of `{name}` must be {}",
                            match ty {
                                Ty::Int => "an integer",
                                Ty::Target => "a target",
                                Ty::Pred => "a predicate (a kind or tag name)",
                            }
                        ),
                    ));
                }
            }
        }
        self.asm.call(idx as u16, width);
        Ok(sub.returns)
    }

    fn stmts(&mut self, body: &[Stmt]) -> Result<()> {
        for s in body {
            self.stmt(s)?;
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
                let Some(i) = self.sub else {
                    return Err(self.err(at, "`return` outside a sub"));
                };
                match (value, self.subs[i].returns) {
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
            Stmt::Idle => {
                self.asm.act(Action::Idle);
            }
            Stmt::Die => {
                self.asm.act(Action::Die);
            }
            Stmt::Become { kind, at } => {
                let id = self
                    .kind_id(kind)
                    .ok_or_else(|| self.err(at, format!("unknown kind `{kind}`")))?;
                self.push_int(i32::from(id));
                self.asm.act(Action::Become);
            }
            Stmt::Spawn {
                kind,
                at,
                pos,
                with,
            } => {
                let id = self
                    .kind_id(kind)
                    .ok_or_else(|| self.err(pos, format!("unknown kind `{kind}`")))?;
                self.push_int(i32::from(id));
                self.target(at)?;
                if let Some((a, b)) = with {
                    self.expr(a)?;
                    self.expr(b)?;
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
                if self.kind.is_none() {
                    return Err(
                        self.err(at, format!("`{verb}` inside a sub: needs belong to a kind"))
                    );
                }
                let slot = self.need_slot(need).ok_or_else(|| {
                    self.err(at, format!("`{verb}`: this kind has no need `{need}`"))
                })?;
                self.target(target)?;
                self.push_int(i32::from(slot));
                self.expr(amount)?;
                self.asm
                    .act(if *give { Action::Give } else { Action::Take });
            }
            Stmt::Move(t) => {
                self.target(t)?;
                self.asm.act(Action::Move);
            }
            Stmt::Drink(t) => {
                self.target(t)?;
                self.asm.act(Action::Drink);
            }
            Stmt::Eat(t) => {
                self.target(t)?;
                self.asm.act(Action::Eat);
            }
            Stmt::Hit(t) => {
                self.target(t)?;
                self.asm.act(Action::Hit);
            }
            Stmt::Graze(t) => {
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
                let Some(k) = self.kind else {
                    return Err(self.err(at, "`next` inside a sub: states belong to a kind"));
                };
                let s = self.kinds[k]
                    .states
                    .iter()
                    .position(|st| st.name == *name)
                    .ok_or_else(|| {
                        self.err(
                            at,
                            format!("kind `{}` has no state `{name}`", self.kinds[k].name),
                        )
                    })?;
                self.asm.next(s as u8);
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
                self.scoped(|g| {
                    let base = g.alloc_local(at, FOR_EACH_LOCALS)?;
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
            let base = g.alloc_local(&here, (n + 1) as u8)?;
            let draw = base + n as u8;
            g.asm.push(0);
            for (i, (w, _)) in arms.iter().enumerate() {
                g.expr(w)?;
                g.asm.push(0).op(OpCode::Max).store(base + i as u8);
                g.asm.load(base + i as u8).op(OpCode::Add);
            }
            g.asm.op(OpCode::Rand).store(draw);
            let end = g.asm.label();
            for (i, (_, body)) in arms.iter().enumerate() {
                let skip = g.asm.label();
                g.asm
                    .load(draw)
                    .load(base + i as u8)
                    .op(OpCode::Lt)
                    .jz(skip);
                g.scoped(|g| g.stmts(body))?;
                g.asm.jmp(end).bind(skip);
                if i + 1 < n {
                    g.asm
                        .load(draw)
                        .load(base + i as u8)
                        .op(OpCode::Sub)
                        .store(draw);
                }
            }
            g.asm.bind(end);
            Ok(())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rules::vm::Op;

    fn compile_ok(text: &str) -> Kinds {
        compile("t.rules", text).unwrap_or_else(|e| panic!("{e}"))
    }

    fn compile_err(text: &str) -> String {
        compile("t.rules", text)
            .err()
            .map(|e| e.to_string())
            .expect("should not compile")
    }

    #[test]
    fn plants_file_compiles_to_the_hand_assembled_program() {
        let text = crate::rules::builtin::ORACLE_PLANTS;
        let compiled = compile("plants.rules", text).unwrap_or_else(|e| panic!("{e}"));
        let expected = crate::rules::builtin::hand_assembled();
        let ops = |k: &Kinds| {
            k.code
                .iter()
                .map(|o| format!("{o:?}"))
                .collect::<Vec<_>>()
                .join("\n")
        };
        assert_eq!(ops(&compiled), ops(&expected));
        assert_eq!(compiled.consts, expected.consts);
        assert_eq!(compiled.defs, expected.defs);
        assert_eq!(compiled.hash, expected.hash);
    }

    #[test]
    fn lexer_handles_times_strings_symbols_and_comments() {
        let toks = Lexer::new(
            "t",
            "kind x { # c\n glyph \"T\" 6h 30min 2d 7 => >= += c.dx }",
        )
        .lex()
        .unwrap();
        let kinds: Vec<Tok> = toks.into_iter().map(|t| t.tok).collect();
        assert_eq!(
            kinds,
            vec![
                Tok::Name("kind".into()),
                Tok::Name("x".into()),
                Tok::Sym("{"),
                Tok::Name("glyph".into()),
                Tok::Str("T".into()),
                Tok::Time(hours(6) as i32),
                Tok::Time(minutes(30) as i32),
                Tok::Time(days(2) as i32),
                Tok::Int(7),
                Tok::Sym("=>"),
                Tok::Sym(">="),
                Tok::Sym("+="),
                Tok::Name("c".into()),
                Tok::Sym("."),
                Tok::Name("dx".into()),
                Tok::Sym("}"),
                Tok::Eof,
            ]
        );
        assert!(compile_err("kind a { glyph \"a\" 3w }").contains("unknown unit"));
        assert!(compile_err("kind a { glyph \"ab\" }").contains("one printable"));
        assert!(
            compile_err("kind a { glyph \"a\" when 1 => x = $ }").contains("unexpected character")
        );
    }

    #[test]
    fn errors_carry_positions_and_name_the_problem() {
        assert_eq!(
            compile_err("kind a {\n  need water max 1h\n  when watter > 0 => idle\n}"),
            "t.rules:3:8: unknown name `watter` (not a need, mem slot or binding of this kind)"
        );
        assert!(compile_err("kind a { cadence 3 }").contains("power of two"));
        assert!(compile_err("kind a { sight 40 }").contains("0 to 16"));
        assert!(compile_err("kind a { need w max 1h need w max 2h }").contains("declared twice"));
        assert!(compile_err("kind a { mem m, m }").contains("declared twice"));
        assert!(compile_err("kind a { when 1 => become b }").contains("unknown kind `b`"));
        assert!(compile_err("kind a { when 1 => spawn a at c }").contains("unknown name `c`"));
        assert!(compile_err("kind a { when 1 => light = 2 }").contains("reserved word"));
        assert!(
            compile_err("kind a { when nearest free within 1 as c or 1 => idle }")
                .contains("top-level conjunct")
        );
        assert!(
            compile_err("kind a { when not nearest free within 1 as c => idle }")
                .contains("top-level conjunct")
        );
        assert!(compile_err("kind a { when 1 => idle } kind a { }").contains("declared twice"));
        assert!(
            compile_err("kind a { when 1 => idle\n glyph \"x\" }")
                .contains("expected `when`, `state` or `}`")
        );
        assert!(compile_err("kind a { when 1 => { idle").contains("unclosed block"));
        assert!(compile_err("kind a { when min(1) > 0 => idle }").contains("takes 2 arguments"));
        assert!(compile_err("kind a { need n max 1h mem n }").contains("declared twice"));
        let many: String = (0..5).map(|i| format!("need n{i} max 1h ")).collect();
        assert!(compile_err(&format!("kind a {{ {many} }}")).contains("at most 4 needs"));
        // The radius binds tighter than a comparison.
        let k = compile_ok("kind a { when count water within 2 > 0 => idle }");
        let ops: Vec<OpCode> = k.code.iter().map(|o| o.code).collect();
        assert_eq!(
            &ops[..5],
            &[
                OpCode::Push,
                OpCode::Push,
                OpCode::Count,
                OpCode::Push,
                OpCode::Gt
            ]
        );
        // Subs and targets.
        assert!(compile_err("kind a { when 1 => f(1) }").contains("unknown sub `f`"));
        assert!(
            compile_err("sub f(n) { } kind a { when 1 => f(1, 2) }")
                .contains("takes 1 arguments, 2 given")
        );
        assert!(
            compile_err("sub f(t: target) { } kind a { when 1 => f(3) }")
                .contains("must be a target")
        );
        assert!(
            compile_err("sub f(n) { } kind a { when f(3) > 0 => idle }")
                .contains("returns nothing")
        );
        assert!(compile_err("sub f(n) { return } sub f(m) { }").contains("sub `f` declared twice"));
        assert!(compile_err("kind a { mem m when 1 => return 3 }").contains("outside a sub"));
        assert!(
            compile_err("sub f() { water = 1 } kind a { need water max 1h }")
                .contains("cannot assign")
        );
        assert!(
            compile_err("sub f() { let a = water } kind a { need water max 1h }")
                .contains("sees only its parameters")
        );
        assert!(
            compile_err("kind a { when nearest free within 1 as c => c = 2 }")
                .contains("not an integer local")
        );
        assert!(
            compile_err("kind a { when nearest free within 1 as c => let v = c }")
                .contains("is a target")
        );
        assert!(compile_err("kind a { when 1 => move toward 3 }").contains("expected a target"));
        assert!(
            compile_err("kind a { when 1 => { let v = 1  let v = 2 } }").contains("already bound")
        );
    }

    #[test]
    fn defaults_and_declarations_land_in_the_def() {
        let k = compile_ok(
            "kind a { }\nkind b { glyph \"b\" cadence 16 sight 7 fuel 99 food 2h bite 3 tags meat feed need w max 3d decay 1 need h max 5 decay 0 vital mem p, q }",
        );
        let a = &k.defs[0];
        assert_eq!(
            (a.glyph, a.cadence_shift, a.sight, a.fuel, a.food, a.bite),
            (b'?', 3, 4, 512, 0, 1)
        );
        let b = &k.defs[1];
        assert_eq!(
            (b.glyph, b.cadence_shift, b.sight, b.fuel, b.food, b.bite),
            (b'b', 4, 7, 99, hours(2) as i32, 3)
        );
        assert_eq!(b.needs.len(), 2);
        assert!(b.needs[0].decays && !b.needs[0].vital);
        assert!(!b.needs[1].decays && b.needs[1].vital);
        assert_eq!(b.mems, vec!["p", "q"]);
        assert_eq!(b.entry, 1); // kind a's program is one Halt
        assert_eq!(k.code[0].code, OpCode::Halt);
    }

    #[test]
    fn conditions_short_circuit_and_bindings_scope_to_the_rule() {
        let k = compile_ok(
            "kind a { mem m\n when (m > 1 or m < -1) and not m == 0 => m = 0\n when nearest free within 1 as c and m > 0 => spawn a at c\n when 1 => if m > 5 { m = 5 } else if m < 0 { m = 0 } else { idle } }",
        );
        let c = &k.code;
        let ops: Vec<OpCode> = c.iter().map(|o| o.code).collect();
        let mut i = 0;
        let expect = |i: &mut usize, want: &[OpCode]| {
            assert_eq!(&ops[*i..*i + want.len()], want, "at op {i}");
            *i += want.len();
        };
        expect(
            &mut i,
            &[
                OpCode::Mem,
                OpCode::Push,
                OpCode::Gt,
                OpCode::Jz,
                OpCode::Jmp,
            ],
        );
        expect(&mut i, &[OpCode::Mem, OpCode::Push, OpCode::Lt, OpCode::Jz]);
        expect(
            &mut i,
            &[
                OpCode::Mem,
                OpCode::Push,
                OpCode::Eq,
                OpCode::Jz,
                OpCode::Jmp,
            ],
        );
        expect(&mut i, &[OpCode::Push, OpCode::SetMem, OpCode::EndRule]);
        expect(
            &mut i,
            &[OpCode::Push, OpCode::Push, OpCode::Nearest, OpCode::Jz],
        );
        expect(&mut i, &[OpCode::Mem, OpCode::Push, OpCode::Gt, OpCode::Jz]);
        expect(
            &mut i,
            &[
                OpCode::Push,
                OpCode::Load,
                OpCode::Load,
                OpCode::Act,
                OpCode::EndRule,
            ],
        );
        assert_eq!(c[i - 4], Op::new(OpCode::Load, 0, 0));
        assert_eq!(c[i - 3], Op::new(OpCode::Load, 1, 0));
        expect(&mut i, &[OpCode::Push, OpCode::Jz]);
        expect(
            &mut i,
            &[
                OpCode::Mem,
                OpCode::Push,
                OpCode::Gt,
                OpCode::Jz,
                OpCode::Push,
                OpCode::SetMem,
                OpCode::Jmp,
            ],
        );
        expect(
            &mut i,
            &[
                OpCode::Mem,
                OpCode::Push,
                OpCode::Lt,
                OpCode::Jz,
                OpCode::Push,
                OpCode::SetMem,
                OpCode::Jmp,
            ],
        );
        expect(&mut i, &[OpCode::Act, OpCode::EndRule, OpCode::Halt]);
        assert_eq!(i, ops.len());
    }

    #[test]
    fn subs_targets_and_loops_compile_and_run() {
        use crate::actors::{ActorMind, ChunkActors};
        use crate::rules::vm::{self, Ctx, Halo};
        use crate::stage::{ChunkCells, Pos as WorldPos};
        use bytemuck::Zeroable;
        let k = compile_ok(
            "sub twice(n) { return n * 2 }
             sub far(t: target) { return dist(t) > 3 }
             sub count_free(r) { let n = 0  let i = 0  while i < r { n += count free within i  i += 1 }  return n }
             sub go(t: target) { if free(toward t) { move toward t } else { move random free } }
             kind a { mem a, b, c, d, e, f
               when 1 => { a = twice(21)  b = far(at(x + 5, y))  c = count_free(2)  d = 0  repeat 4 { d += 3 } }
               when nearest water within 8 as w => { e = w.dx * 100 + w.dy  f = is(w, water) + dist(w) * 10 }
               when 1 => go(north) }",
        );
        assert_eq!(k.subs.len(), 4);
        let mut cells = ChunkCells::default();
        cells.ground[10 * 64 + 13] = Ground::Water; // 2 east of the actor at (11, 10)
        let actors = ChunkActors::default();
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
            tags: &k.tag_bits,
        };
        let mut mind = ActorMind::zeroed();
        let ctx = Ctx {
            halo: &halo,
            kind: &k.defs[0],
            cell: 10 * 64 + 11,
            pos: WorldPos::new(11, 10),
            tick: 5,
            rng: vm::rng_base(1, 5, 9),
            look: 0,
            signal: 0,
        };
        // Rule 1 has no action: falls through; rule 2 neither; rule 3 moves.
        let out = vm::think(&k, ctx, &mut mind);
        assert_eq!(out.trap, None, "{out:?}");
        assert_eq!(out.action, Action::Move);
        assert_eq!((out.dx, out.dy), (0, -1));
        // count_free(2): radius 0 is the 1x1 square (nobody stands in this
        // bare test's occupant array, so 1), radius 1 the 3x3 (9). Total 10.
        assert_eq!(&mind.mem[..6], &[42, 1, 10, 12, 200, 1 + 20]);
    }

    #[test]
    fn states_consts_looks_signals_and_for_each_compile_and_run() {
        use crate::actors::{ActorMind, ActorPub, ChunkActors};
        use crate::rules::vm::{self, Ctx, Halo};
        use crate::stage::{ActorId, ChunkCells, Pos as WorldPos};
        use bytemuck::Zeroable;
        let k = compile_ok(
            "const LOAD = 30min
             const TWICE = LOAD * 2 + min(1, 2)
             sub tally(what: pred) { let n = 0  for each what within 3 as f { n += 1 + look_of(f) }  return n }
             kind a { mem s, n, m, p, h, l
               when s == 1 => { s = 2  next B }
               state A {
                 when true => { n = tally(a:3)  m = tally(a)  p = pack(-3, 5)  h = hi(p)  l = lo(p)
                                s = TWICE  signal = p  look = 4  next B }
               }
               state B {
                 when nearest a:3 within 2 as f => { s = signal_of(f)  idle }
                 when true => die
               } }",
        );
        assert_eq!(k.defs[0].states, 2);
        // Self at (10, 10); a:3 two east, a:0 one north-west, a:3 out of reach.
        let mut cells = ChunkCells::default();
        let mut actors = ChunkActors::default();
        for (slot, (x, y, look, signal)) in [
            (10, 10, 0, 0),
            (12, 10, 3, 77),
            (9, 9, 0, 0),
            (20, 20, 3, 0),
        ]
        .into_iter()
        .enumerate()
        {
            let cell = y * 64 + x;
            cells.occupant[cell] = ActorId::pack(0, slot as u16);
            actors.rows.push(ActorPub {
                cell: cell as u16,
                look,
                signal,
                ..ActorPub::zeroed()
            });
        }
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
            tags: &k.tag_bits,
        };
        let ctx = Ctx {
            halo: &halo,
            kind: &k.defs[0],
            cell: 10 * 64 + 10,
            pos: WorldPos::new(10, 10),
            tick: 5,
            rng: vm::rng_base(1, 5, 9),
            look: 0,
            signal: 0,
        };
        // State A (the first): the loops, the bytes, the effects, `next`.
        let mut mind = ActorMind::zeroed();
        let out = vm::think(&k, ctx, &mut mind);
        assert_eq!(out.trap, None, "{out:?}");
        assert_eq!(out.action, Action::Idle);
        assert_eq!(
            (out.next, out.look, out.signal),
            (Some(1), Some(4), Some(-763))
        );
        // tally(a:3) = 1 + 3; tally(a) = (1 + 3) + (1 + 0); pack/hi/lo round-trip.
        assert_eq!(&mind.mem[..6], &[901, 4, 5, -763, -3, 5]);
        // State B: reads the neighbour's signal through `a:3`.
        mind.state = 1;
        let out = vm::think(&k, ctx, &mut mind);
        assert_eq!((out.trap, out.action, out.next), (None, Action::Idle, None));
        assert_eq!(mind.mem[0], 77);
        // The reflex runs first, in any state.
        mind.mem[0] = 1;
        let out = vm::think(&k, ctx, &mut mind);
        assert_eq!((out.next, mind.mem[0]), (Some(1), 2));
        // Nothing to see: B falls to `die`.
        let empty = ChunkActors::default();
        let bare = ChunkCells::default();
        let halo = Halo {
            chunks: [
                None,
                None,
                None,
                None,
                Some((&bare, &empty)),
                None,
                None,
                None,
                None,
            ],
            tags: &k.tag_bits,
        };
        let out = vm::think(&k, Ctx { halo: &halo, ..ctx }, &mut mind);
        assert_eq!(out.action, Action::Die);

        let errs = [
            (
                "kind a { state S { when true => next T } }",
                "has no state `T`",
            ),
            (
                "sub f() { next S }  kind a { state S { when true => f() } }",
                "`next` inside a sub",
            ),
            (
                "kind a { mem m  when true => m = 1 }  const C = m",
                "not a constant declared above",
            ),
            ("const C = 1  const C = 2", "already a name"),
            (
                "kind a { tags t  when nearest t:1 within 2 as v => idle }",
                "`t` is not a kind",
            ),
            (
                "kind a { state S { } state S { } }",
                "state `S` declared twice",
            ),
        ];
        for (text, want) in errs {
            let e = compile_err(text);
            assert!(e.contains(want), "{text}: {e}");
        }
    }

    #[test]
    fn choose_draws_once_and_large_constants_use_the_pool() {
        let k = compile_ok(
            "kind a { mem m\n when 1 => choose { 3: m = 1  2: m = 2 }\n when m > 100000 => m = 3d }",
        );
        assert_eq!(k.consts, vec![100_000, days(3) as i32]);
        let rand = k.code.iter().filter(|o| o.code == OpCode::Rand).count();
        assert_eq!(rand, 1);
        use crate::actors::ChunkActors;
        use crate::rules::vm::{self, Ctx, Halo};
        use crate::stage::{ChunkCells, Pos as WorldPos};
        use bytemuck::Zeroable;
        let cells = ChunkCells::default();
        let actors = ChunkActors::default();
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
            tags: &k.tag_bits,
        };
        let mut counts = [0; 3];
        for uid in 0..500u64 {
            let mut mind = crate::actors::ActorMind::zeroed();
            let ctx = Ctx {
                halo: &halo,
                kind: &k.defs[0],
                cell: 100,
                pos: WorldPos::new(36, 1),
                tick: 77,
                rng: vm::rng_base(3, 77, uid),
                look: 0,
                signal: 0,
            };
            let out = vm::think(&k, ctx, &mut mind);
            assert_eq!(out.trap, None);
            counts[mind.mem[0] as usize] += 1;
        }
        assert_eq!(counts[0], 0);
        assert!(counts[1] > 240 && counts[1] < 360, "{counts:?}");
        assert!(counts[2] > 140 && counts[2] < 260, "{counts:?}");
    }

    #[test]
    fn a_directory_compiles_in_sorted_file_order() {
        let dir = std::env::temp_dir().join(format!("wmc-rules-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("b.rules"), "kind bee { when 1 => become ant }").unwrap();
        std::fs::write(dir.join("a.rules"), "kind ant { }").unwrap();
        std::fs::write(dir.join("notes.txt"), "kind ignored { }").unwrap();
        let k = compile_dir(&dir).unwrap();
        assert_eq!(k.names().collect::<Vec<_>>(), vec!["ant", "bee"]);
        std::fs::write(dir.join("c.rules"), "kind ant { }").unwrap();
        let err = compile_dir(&dir).unwrap_err().to_string();
        assert!(
            err.starts_with("c.rules:1:6: kind `ant` declared twice"),
            "{err}"
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
