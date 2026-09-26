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
use super::{DEFAULT_COLOR, DebugInfo, Diagnostic, KindDef, Kinds, Level, NeedDef, RuleInfo};
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
    Gen::new(&items, files).generate()
}

/// Compile rule packs as one rule set. A pack is a directory (its `*.rules`
/// files, in sorted file-name order) or a single file; packs go in the
/// order given, with one namespace across all of them (a name declared in
/// two packs is an error naming both). With more than one pack, a file is
/// named `pack/file.rules` in positions, `pack` being the directory's own
/// name. The packs' absolute paths go in `debug.packs`: a save remembers
/// them.
pub fn compile_packs(
    packs: &[&std::path::Path],
) -> std::result::Result<Kinds, Box<dyn std::error::Error>> {
    let mut texts: Vec<(String, String)> = Vec::new();
    let mut abs = Vec::with_capacity(packs.len());
    for pack in packs {
        let full = std::fs::canonicalize(pack).map_err(|e| format!("{}: {e}", pack.display()))?;
        let files = if full.is_dir() {
            let mut v: Vec<_> = std::fs::read_dir(&full)
                .map_err(|e| format!("{}: {e}", pack.display()))?
                .filter_map(|e| e.ok().map(|e| e.path()))
                .filter(|p| p.extension().is_some_and(|x| x == "rules"))
                .collect();
            v.sort();
            v
        } else {
            vec![full.clone()]
        };
        let dir_name = full
            .file_name()
            .filter(|_| full.is_dir() && packs.len() > 1)
            .map(|n| n.to_string_lossy().into_owned());
        for p in files {
            let file = p
                .file_name()
                .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
            let name = match &dir_name {
                Some(d) => format!("{d}/{file}"),
                None => file,
            };
            let text = std::fs::read_to_string(&p).map_err(|e| format!("{}: {e}", p.display()))?;
            texts.push((name, text));
        }
        abs.push(full.to_string_lossy().into_owned());
    }
    let files: Vec<(&str, &str)> = texts
        .iter()
        .map(|(n, t)| (n.as_str(), t.as_str()))
        .collect();
    let mut kinds = compile_files(&files)?;
    kinds.debug.packs = abs;
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
    "trait",
    "extends",
    "inherit",
    "only",
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

    fn int(&mut self, what: &str) -> Result<i32> {
        match self.bump() {
            Tok::Int(v) => Ok(v),
            _ => {
                self.at -= 1;
                Err(self.err(format!("expected {what}, found {}", self.describe())))
            }
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
            if states.len() == 64 {
                return Err(self.err_at(&sat, "at most 64 states per kind"));
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
                name: sname,
                rules: srules,
            });
        }
        if !self.is_sym("}") {
            let what = if DECL_WORDS.iter().any(|w| self.is_kw(w)) {
                "declarations come first, before subs and rules".to_string()
            } else if self.is_kw("sub") {
                "member subs come before the rules".to_string()
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
                while let Tok::Name(n) = self.peek().clone() {
                    if is_reserved(&n) {
                        break;
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
                    Tok::Str(s) if s.len() == 7 && s.starts_with('#') => {
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
            let body = self.body()?;
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
        let only = self.eat_kw("only");
        let not_a_kind = |what: &str| format!("`only` applies to a kind, not `{what}`");
        for (word, p) in [
            ("free", Pred::Free),
            ("bare", Pred::Bare),
            ("water", Pred::Ground(Ground::Water)),
            ("soil", Pred::Ground(Ground::Soil)),
            ("rock", Pred::Feature(Feature::Rock)),
        ] {
            if self.eat_kw(word) {
                if only {
                    return Err(self.err_at(&at, not_a_kind(word)));
                }
                return Ok(p);
            }
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
    locals: Vec<Local>,
    next_local: u8,
    /// Position for errors without a better one.
    here: Pos,
    /// Tag names, in first-appearance order (item order, then declaration
    /// order): a tag's bit is its index here.
    tags: Vec<String>,
    /// Scent channel names, in first-appearance order in the code.
    scents: Vec<String>,
    /// The source texts, for the rule table in [`DebugInfo`].
    files: &'a [(&'a str, &'a str)],
    debug: DebugInfo,
    /// The state whose rules are being compiled (`None`: the reflexes).
    state: Option<u8>,
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
        _ => None,
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
            asm: Asm::new(),
            pool: Vec::new(),
            kind: None,
            cur: None,
            owner: None,
            members_here: Vec::new(),
            sub: None,
            params: Vec::new(),
            checking: false,
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
        }
        let member_subs = items.iter().flat_map(|it| it.members.iter());
        for s in self.subs.iter().chain(member_subs) {
            let width: u32 = s.params.iter().map(|(_, t)| u32::from(t.width())).sum();
            if width > FRAME_LOCALS as u32 {
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
            }
        }
        // Tags: global names, a bit each, never a kind's or a trait's name.
        for it in items {
            for t in &it.decls.tags {
                if let Some(o) = self.item_named(t) {
                    let what = if items[o].is_trait { "trait" } else { "kind" };
                    return Err(self.err(&it.at, format!("tag `{t}` is also a {what}'s name")));
                }
                if !self.tags.contains(t) {
                    if self.tags.len() == 64 {
                        return Err(self.err(&it.at, "at most 64 tags in a rule set"));
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
            let v = self.fold(&c.value)?;
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
                let idx = u16::try_from(next_sub)
                    .map_err(|_| self.err(&items[self.insts[ki].item].at, "too many subs"))?;
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
        self.diagnose();

        let code = std::mem::take(&mut self.asm).finish();
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
        Ok(Kinds::from_parts(defs, code, self.pool, sub_entries)
            .with_scents(self.scents)
            .with_debug(self.debug))
    }

    // ---- inheritance ------------------------------------------------------------------

    /// Resolve `item` with `args` (a trait's arguments; none for a kind):
    /// its ancestors, merged declarations and rule lists. Memoized per
    /// (item, arguments).
    fn inst(&mut self, item: usize, args: Vec<i32>, stack: &mut Vec<usize>) -> Result<usize> {
        if let Some(i) = self
            .insts
            .iter()
            .position(|x| x.item == item && x.args == args)
        {
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
            let folded: Result<Vec<i32>> = p.args.iter().map(|a| self.fold(a)).collect();
            self.params = saved;
            let pinst = self.inst(pi, folded?, stack)?;
            parents.push(pinst);
        }
        stack.pop();
        // Linearize: each parent's ancestors, then the parent; the first
        // occurrence wins. One trait, two argument lists: ambiguous.
        let mut ancestors: Vec<usize> = Vec::new();
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
        let me = self.insts.len();
        let saved = std::mem::replace(&mut self.params, scope);
        let inst = self.merge(item, args, parents, ancestors, me);
        self.params = saved;
        let inst = inst?;
        debug_assert_eq!(self.insts.len(), me, "merge resolves nothing new");
        self.insts.push(inst);
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
        let x = self.fold(e)?;
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
            .map(|c| c.trailing_zeros() as u8);
        let sight = self
            .decl_num(
                &d.sight,
                |s| (0..=16).contains(&s),
                "sight is 0 to 16 cells",
            )?
            .map(|s| s as u8);
        let fuel = self
            .decl_num(
                &d.fuel,
                |f| (1..=4096).contains(&f),
                "fuel is 1 to 4096 ops per think",
            )?
            .map(|f| f as u32);
        let food = self.decl_num(&d.food, |_| true, "")?;
        let bite = self
            .decl_num(&d.bite, |b| (0..=255).contains(&b), "bite is 0 to 255")?
            .map(|b| b as u8);
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
            let max = self.fold(&na.max)?;
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
        if states.len() > 64 {
            return Err(self.err(&it.at, format!("`{}` has more than 64 states", it.name)));
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
            self.item_ids[k] = Some(id as u16);
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
        self.kind = Some(k as u16);
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
            self.state = Some(s as u8);
            let skip = self.asm.label();
            self.asm
                .sense(Sense::State)
                .push(s as i32)
                .op(OpCode::Eq)
                .jz(skip);
            self.rule_list(list)?;
            self.asm.halt().bind(skip);
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
            id: k as u16,
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
            states: inst.states.len().max(1) as u8,
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
            let ti = self.inst(i, vec![1; it.params.len()], &mut Vec::new())?;
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
                self.state = Some(s as u8);
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

    /// Warnings and notes about the compiled kinds, in kind order.
    fn diagnose(&mut self) {
        let items = self.items;
        let mut out: Vec<Diagnostic> = Vec::new();
        let push = |out: &mut Vec<Diagnostic>, level: Level, at: &Pos, msg: String| {
            let d = Diagnostic {
                level,
                file: at.file.clone(),
                line: at.line,
                col: at.col,
                msg,
            };
            if !out.contains(&d) {
                out.push(d);
            }
        };
        for k in 0..self.kind_insts.len() {
            self.enter(k);
            let ki = self.kind_insts[k];
            let inst = self.insts[ki].clone();
            // A rule that always holds and always ends the think hides every
            // rule after it (and, among the reflexes, every state's rules).
            let mut reflex_end: Option<&'a Rule> = None;
            let lists: Vec<&RList<'a>> = [&inst.reflex]
                .into_iter()
                .chain(&inst.state_lists)
                .collect();
            for (li, list) in lists.iter().enumerate() {
                if li > 0 {
                    if let (Some(end), Some(first)) = (reflex_end, list.rules.first()) {
                        push(
                            &mut out,
                            Level::Warning,
                            &first.rule.at,
                            format!(
                                "never runs: the reflex rule at {}:{} always ends the think",
                                end.at.file, end.at.line
                            ),
                        );
                    }
                    if reflex_end.is_some() {
                        continue;
                    }
                }
                for (i, rr) in list.rules.iter().enumerate() {
                    self.owner = Some(rr.owner);
                    self.params = self.scope_of(rr.owner);
                    let always = matches!(&rr.rule.cond, Cond::Expr(e) if self.fold(e).is_ok_and(|v| v != 0));
                    if always && self.ends_all(&rr.rule.body, false, 0) {
                        if let Some(next) = list.rules.get(i + 1) {
                            push(
                                &mut out,
                                Level::Warning,
                                &next.rule.at,
                                format!(
                                    "never runs: the rule at {}:{} always ends the think",
                                    rr.rule.at.file, rr.rule.at.line
                                ),
                            );
                        }
                        if li == 0 {
                            reflex_end = Some(rr.rule);
                        }
                        break;
                    }
                }
            }
            // Ancestors whose reflex rules this kind does not run.
            for &a in &inst.ancestors {
                let has_rules = items[self.insts[a].item]
                    .rules
                    .iter()
                    .any(|r| matches!(r, RuleItem::When(_)));
                if has_rules && !inst.reflex.sources.contains(&a) {
                    let it = &items[inst.item];
                    push(
                        &mut out,
                        Level::Note,
                        &it.at,
                        format!(
                            "`{}` does not run the reflex rules of `{}` (no `inherit` splices them)",
                            it.name,
                            self.inst_name(a)
                        ),
                    );
                }
            }
        }
        self.owner = None;
        self.params.clear();
        self.debug.diagnostics = out;
    }

    /// Does this statement end the think on every path: an action (and, if
    /// not `acts_only`, a `next`)? Conservative: loops never count, and a
    /// call counts only through subs that do, eight calls deep.
    fn ends(&self, s: &Stmt, acts_only: bool, depth: u32) -> bool {
        match s {
            Stmt::Next(..) => !acts_only,
            Stmt::If { then, els, .. } => {
                !els.is_empty()
                    && self.ends_all(then, acts_only, depth)
                    && self.ends_all(els, acts_only, depth)
            }
            Stmt::Choose(arms) => {
                !arms.is_empty()
                    && arms.iter().all(|(w, body)| {
                        self.fold(w).is_ok_and(|w| w > 0) && self.ends_all(body, acts_only, depth)
                    })
            }
            Stmt::Call { name, .. } => {
                depth < 8
                    && self
                        .callee(name)
                        .is_some_and(|sub| self.ends_all(&sub.body, acts_only, depth + 1))
            }
            _ => action_at(s).is_some(),
        }
    }

    fn ends_all(&self, body: &[Stmt], acts_only: bool, depth: u32) -> bool {
        body.iter().any(|s| self.ends(s, acts_only, depth))
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
        self.cond(&rule.cond, next, true)?;
        let body_pc = self.asm.here();
        self.stmts(&rule.body)?;
        self.asm.end_rule().bind(next);
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
            file: file as u16,
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
                .param(n)
                .or_else(|| self.const_value(n))
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
            .map(|i| i as u8)
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
            .map(|i| i as u8)
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
                    pred::TAG_BASE + bit as i32
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
                } else if let Some(v) = self.param(n).or_else(|| self.const_value(n)) {
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
            (i as u16, &subs[i])
        };
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
                (Ty::Pred, Arg::Name(n, p)) => {
                    self.pred(&Pred::Kind(n.clone(), false, p.clone()))?
                }
                (Ty::Pred, Arg::Pred(pr)) => self.pred(pr)?,
                (Ty::Pred, Arg::Expr(Expr::Name(n, p))) => {
                    self.pred(&Pred::Kind(n.clone(), false, p.clone()))?;
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
        self.asm.call(idx, width);
        Ok(sub.returns)
    }

    fn stmts(&mut self, body: &[Stmt]) -> Result<()> {
        // A statement that acts on every path, then another action in the
        // same list: a second action, certain; refused here rather than
        // trapped at run time.
        let mut acted: Option<Option<u32>> = None;
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
            self.stmt(s)?;
            if acted.is_none() && self.ends(s, true, 0) {
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
                self.push_int(i32::from(id));
                self.asm.act(Action::Become);
            }
            Stmt::Spawn {
                kind,
                at,
                pos,
                with,
            } => {
                let id = self.concrete(kind, pos)?;
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
                if self.cur.is_none() {
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
                .contains("declarations come first")
        );
        assert!(compile_err("kind a { when 1 => { idle").contains("unclosed block"));
        assert!(compile_err("kind a { when min(1) > 0 => idle }").contains("takes 2 arguments"));
        assert!(compile_err("kind a { need n max 1h mem n }").contains("declared twice"));
        let many: String = (0..5).map(|i| format!("need n{i} max 1h ")).collect();
        assert!(compile_err(&format!("kind a {{ {many} }}")).contains("has 5 needs, at most 4"));
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
            family_end: &k.family_end,
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
            family_end: &k.family_end,
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
            family_end: &k.family_end,
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
            (
                "const C = 1  const C = 2",
                "const `C` declared twice (first at t.rules:1)",
            ),
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
            family_end: &k.family_end,
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

    /// Lines of kind `k`'s rules in scan order, with the trait or parent
    /// each came from.
    fn rule_lines(k: &Kinds, kind: &str) -> Vec<(Option<u8>, u32, Option<String>)> {
        let id = k.by_name(kind).unwrap().id;
        k.debug
            .rules
            .iter()
            .filter(|r| r.kind == id)
            .map(|r| (r.state, r.line, r.via.clone()))
            .collect()
    }

    /// A halo over one bare chunk, for running a think in a test.
    fn run_think(
        k: &Kinds,
        kind: &str,
        mind: &mut crate::actors::ActorMind,
    ) -> crate::rules::vm::Outcome {
        use crate::actors::ChunkActors;
        use crate::rules::vm::{self, Ctx, Halo};
        use crate::stage::{ChunkCells, Pos as WorldPos};
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
            family_end: &k.family_end,
        };
        let ctx = Ctx {
            halo: &halo,
            kind: k.by_name(kind).unwrap(),
            cell: 100,
            pos: WorldPos::new(36, 1),
            tick: 5,
            rng: vm::rng_base(1, 5, 9),
            look: 0,
            signal: 0,
        };
        vm::think(k, ctx, mind)
    }

    #[test]
    fn traits_merge_declarations_in_linearized_order() {
        let k = compile_ok(
            "trait mover { cadence 2  sight 6  tags animal  need food max 1d vital  mem heading }
             trait drinker(t) { tags thirsty  need water max t vital  mem knows_water }
             kind hen extends mover, drinker(4h) {
               glyph \"h\"  sight 8  need health max 20 decay 0 vital  mem last_egg }
             kind chick extends hen { glyph \"c\"  need water max 2h vital }",
        );
        let hen = k.by_name("hen").unwrap();
        assert_eq!((hen.glyph, hen.cadence_shift, hen.sight), (b'h', 1, 8));
        let needs: Vec<(&str, i32)> = hen.needs.iter().map(|n| (n.name.as_str(), n.max)).collect();
        assert_eq!(needs, [("food", 21600), ("water", 3600), ("health", 20)]);
        assert_eq!(hen.mems, ["heading", "knows_water", "last_egg"]);
        assert_eq!(hen.tags, 0b11);
        assert_eq!(hen.parent, None);
        // The chick keeps every slot where the hen has it; its water is its own.
        let chick = k.by_name("chick").unwrap();
        let needs: Vec<(&str, i32)> = chick
            .needs
            .iter()
            .map(|n| (n.name.as_str(), n.max))
            .collect();
        assert_eq!(needs, [("food", 21600), ("water", 1800), ("health", 20)]);
        assert_eq!((chick.glyph, chick.sight, chick.tags), (b'c', 8, 0b11));
        assert_eq!(chick.parent, Some(hen.id));
        assert_eq!(k.debug.traits, ["mover", "drinker"]);
        assert_eq!(
            k.debug.parents[usize::from(hen.id)],
            ["mover", "drinker(3600)"]
        );
        // A parent's tables change nothing for kinds without parents.
        assert_eq!(k.family_end[usize::from(hen.id)], chick.id + 1);
    }

    #[test]
    fn inherit_splices_where_written_and_appends_when_absent() {
        let k = compile_ok(
            "trait t1 { when hour == 1 => idle }
             trait t2 { when hour == 2 => idle }
             kind a extends t1, t2 {
               when hour == 3 => idle
               inherit t2
               when hour == 4 => idle
             }
             kind b extends t1, t2 { when hour == 5 => idle }
             kind c extends a { when hour == 6 => idle  inherit }",
        );
        let t = |v: Option<&str>| v.map(str::to_string);
        assert_eq!(
            rule_lines(&k, "a"),
            [(None, 4, None), (None, 2, t(Some("t2"))), (None, 6, None)]
        );
        assert_eq!(
            rule_lines(&k, "b"),
            [
                (None, 8, None),
                (None, 1, t(Some("t1"))),
                (None, 2, t(Some("t2")))
            ]
        );
        // `inherit` alone splices the parent's list as the parent runs it:
        // without t1, which `a` left out.
        assert_eq!(
            rule_lines(&k, "c"),
            [
                (None, 9, None),
                (None, 4, t(Some("a"))),
                (None, 2, t(Some("t2"))),
                (None, 6, t(Some("a")))
            ]
        );
        let notes: Vec<&str> = k
            .debug
            .diagnostics
            .iter()
            .filter(|d| d.level == Level::Note)
            .map(|d| d.msg.as_str())
            .collect();
        assert_eq!(
            notes,
            [
                "`a` does not run the reflex rules of `t1` (no `inherit` splices them)",
                "`c` does not run the reflex rules of `t1` (no `inherit` splices them)"
            ]
        );
    }

    #[test]
    fn state_blocks_merge_by_name() {
        let k = compile_ok(
            "trait walker {
               state WALK { when hour == 1 => idle }
               state REST { when hour == 2 => idle }
             }
             kind k extends walker {
               state REST { when hour == 3 => idle  inherit }
               state EAT { when hour == 4 => next WALK }
             }",
        );
        let id = usize::from(k.by_name("k").unwrap().id);
        assert_eq!(k.debug.states[id], ["WALK", "REST", "EAT"]);
        assert_eq!(k.defs[id].states, 3);
        let w = |v: &str| Some(v.to_string());
        assert_eq!(
            rule_lines(&k, "k"),
            [
                (Some(0), 2, w("walker")),
                (Some(1), 6, None),
                (Some(1), 3, w("walker")),
                (Some(2), 7, None)
            ]
        );
        // `next WALK` in the kind's own state is state 0.
        let next = k.code.iter().find(|o| o.code == OpCode::Next).unwrap();
        assert_eq!(next.a, 0);
    }

    #[test]
    fn trait_parameters_fold_per_instantiation() {
        use crate::actors::ActorMind;
        use bytemuck::Zeroable;
        let k = compile_ok(
            "const HOUR = 1h
             trait thirsty(t) { need water max t * 2 vital  when water < t => water = t }
             kind a extends thirsty(2 * HOUR) { }
             kind b extends thirsty(6h) { }",
        );
        assert_eq!(k.by_name("a").unwrap().needs[0].max, 3600);
        assert_eq!(k.by_name("b").unwrap().needs[0].max, 10800);
        for (kind, want) in [("a", 1800), ("b", 5400)] {
            let mut m = ActorMind::zeroed();
            let out = run_think(&k, kind, &mut m);
            assert_eq!(out.trap, None);
            assert_eq!(m.needs[0], want, "{kind}");
        }
        assert!(
            compile_err("trait t(v) { when 1 => v = 2 } kind a extends t(1) { }")
                .contains("cannot assign to `v`: it is a constant")
        );
    }

    #[test]
    fn member_subs_see_their_kinds_needs_and_compile_per_kind() {
        use crate::actors::ActorMind;
        use bytemuck::Zeroable;
        let k = compile_ok(
            "trait eater { need food max 1d vital }
             trait sipper {
               need water max 4h vital
               sub sip(n) { water = water + n }
               when water < 1h => { sip(30min)  idle }
             }
             kind a extends eater, sipper { }
             kind b extends sipper { }
             kind c extends sipper { sub sip(n) { water = 3h } }",
        );
        // Water is slot 1 in `a`, slot 0 in `b` and `c`: one sub, compiled per kind.
        assert_eq!(k.by_name("a").unwrap().need_named("water"), Some(1));
        assert_eq!(k.by_name("b").unwrap().need_named("water"), Some(0));
        for (kind, slot, want) in [("a", 1, 450), ("b", 0, 450), ("c", 0, 2700)] {
            let mut m = ActorMind::zeroed();
            m.needs = [100; 4];
            m.needs[slot] = 0;
            let out = run_think(&k, kind, &mut m);
            assert_eq!(out.trap, None, "{kind}");
            assert_eq!(m.needs[slot], want, "{kind}");
        }
        assert!(k.debug.subs.contains(&"a::sip".to_string()));
        assert!(k.debug.subs.contains(&"c::sip".to_string()));
    }

    #[test]
    fn family_numbering_is_preorder_by_file_then_declaration() {
        let k = compile_ok(
            "kind hen extends bird { }
             kind animal { }
             kind plant { }
             kind bird extends animal { }
             kind fox extends animal { }
             kind tree extends plant { }",
        );
        assert_eq!(
            k.names().collect::<Vec<_>>(),
            ["animal", "bird", "hen", "fox", "plant", "tree"]
        );
        assert_eq!(k.family_end, [4, 3, 3, 4, 6, 6]);
        let parents: Vec<Option<u16>> = k.defs.iter().map(|d| d.parent).collect();
        assert_eq!(parents, [None, Some(0), Some(1), Some(0), None, Some(4)]);
        // `only` and families in predicates.
        let k = compile_ok(
            "kind animal { }
             kind bird extends animal {
               when nearest animal within 1 as a => idle
               when nearest only animal within 1 as a => idle
               when nearest bird:2 within 1 as a => idle
               when nearest only bird:2 within 1 as a => idle
             }",
        );
        let pushes: Vec<i32> = k
            .code
            .windows(3)
            .filter(|w| w[2].code == OpCode::Nearest)
            .map(|w| match w[0].code {
                OpCode::PushK => k.consts[w[0].imm as u16 as usize],
                _ => i32::from(w[0].imm),
            })
            .collect();
        assert_eq!(
            pushes,
            [
                0,
                pred::ONLY,
                pred::kind_look(1, 2),
                pred::kind_look(1, 2) + pred::ONLY
            ]
        );
    }

    #[test]
    fn extends_errors_name_the_problem() {
        let errs = [
            ("kind a extends zz { }", "unknown trait or kind `zz`"),
            (
                "kind a extends b { } kind b extends a { }",
                "extends itself: a -> b -> a",
            ),
            (
                "trait a extends b { } trait b extends a { }",
                "extends itself",
            ),
            (
                "kind a { } kind b { } kind c extends a, b { }",
                "extends two kinds, `a` and `b`",
            ),
            (
                "kind a { } trait t extends a { }",
                "trait `t` can extend only traits",
            ),
            (
                "trait t(v) { } kind a extends t { }",
                "takes 1 argument, 0 given",
            ),
            (
                "kind a { } kind b extends a(1) { }",
                "kind `a` takes no arguments",
            ),
            (
                "trait t(v) { } trait u extends t(1) { } kind a extends u, t(2) { }",
                "reaches trait `t` twice, with (1) and (2)",
            ),
            (
                "trait t { } kind a { inherit t }",
                "`t` is not an ancestor of `a`",
            ),
            (
                "trait t { when true => idle } kind a extends t { inherit t  inherit t }",
                "`inherit t` twice",
            ),
            (
                "trait t { } kind a extends t { state S { inherit t } }",
                "`t` has no state `S`",
            ),
            ("trait t { glyph \"x\" }", "a trait has no glyph"),
            ("trait t { color \"#ffffff\" }", "a trait has no colour"),
            ("kind a(v) { }", "a kind takes no parameters"),
            (
                "trait t { when food < 1 => idle } kind a extends t { need food max 1d }",
                "`t` uses `food`, which it does not declare",
            ),
            ("trait t { when zz > 1 => idle }", "unknown name `zz`"),
            (
                "trait t { } kind a { when nearest t within 1 as v => idle }",
                "`t` is a trait",
            ),
            (
                "trait t { } kind a { when 1 => spawn t at north }",
                "only a kind can be spawned",
            ),
            (
                "kind a { tags z  when nearest only z within 1 as v => idle }",
                "`only` applies to a kind",
            ),
            (
                "kind a { when nearest only water within 1 as v => idle }",
                "`only` applies to a kind",
            ),
            (
                "trait t { when 1 => f() } kind a extends t { sub f() { idle } }",
                "`t` calls `f`, which it does not define",
            ),
            (
                "sub f() { } kind a { sub f() { } }",
                "has the name of a file sub",
            ),
            ("const X = 1  trait t(X) { }", "hides the constant `X`"),
            (
                "trait p { need w max 1h } trait q { need w max 2h } kind a extends p, q { }",
                "inherits need `w` from `p` and `q`",
            ),
            (
                "trait p { cadence 2 } trait q { cadence 4 } kind a extends p, q { }",
                "inherits different cadences from `p` and `q`",
            ),
            (
                "trait p { sub f() { } } trait q { sub f() { } } kind a extends p, q { }",
                "inherits sub `f` from both `p` and `q`",
            ),
            (
                "trait t { when 1 => next S } kind a extends t { state S { } }",
                "trait `t` has no state `S`",
            ),
            ("trait t { } kind t { }", "kind `t` declared twice"),
            (
                "trait t { need a max 1h need b max 1h need c max 1h need d max 1h need e max 1h }",
                "has 5 needs",
            ),
            (
                "kind a { when 1 => idle  sub f() { } }",
                "member subs come before the rules",
            ),
        ];
        for (text, want) in errs {
            let e = compile_err(text);
            assert!(e.contains(want), "{text}\n  got: {e}");
        }
        // A redeclaration settles parents that disagree.
        compile_ok(
            "trait p { need w max 1h } trait q { need w max 2h } kind a extends p, q { need w max 3h }",
        );
        compile_ok("trait p { cadence 2 } trait q { cadence 4 } kind a extends p, q { cadence 8 }");
    }

    #[test]
    fn straight_line_second_action_is_an_error() {
        for text in [
            "kind a { when 1 => { idle  move north } }",
            "kind a { when 1 => { if hour > 1 { idle } else { die }  move north } }",
            "sub f() { idle } kind a { when 1 => { f()  move north } }",
            "kind a { when 1 => { choose { 1: idle  2: die }  move north } }",
        ] {
            let e = compile_err(text);
            assert!(e.contains("a second action"), "{text}: {e}");
        }
        let e = compile_err("kind a {\n when 1 => {\n idle\n move north } }");
        assert!(e.starts_with("t.rules:4:2:"), "{e}");
        assert!(e.contains("already acted at line 3"), "{e}");
        // Conservative: a branch that may not act, or a `next`, is fine.
        compile_ok("kind a { when 1 => { if hour > 1 { idle }  move north } }");
        compile_ok("kind a { when 1 => { next S  move north } state S { } }");
        compile_ok("kind a { when 1 => { choose { 1: idle  0: look = 1 }  move north } }");
    }

    #[test]
    fn unreachable_rule_warning_is_conservative() {
        let warnings = |text: &str| -> Vec<String> {
            compile_ok(text)
                .debug
                .diagnostics
                .iter()
                .filter(|d| d.level == Level::Warning)
                .map(ToString::to_string)
                .collect()
        };
        assert_eq!(
            warnings("kind a { when true => idle\n when hour > 1 => die }"),
            ["t.rules:2:2: warning: never runs: the rule at t.rules:1 always ends the think"]
        );
        assert!(warnings("kind a { when true => look = 1\n when hour > 1 => die }").is_empty());
        assert!(warnings("kind a { when hour > 1 => idle\n when true => die }").is_empty());
        assert!(
            warnings("kind a { when true => { if hour > 1 { idle } }\n when 1 => die }").is_empty()
        );
        assert_eq!(
            warnings("kind a { when true => idle\n state S { when 1 => die } }"),
            [
                "t.rules:2:12: warning: never runs: the reflex rule at t.rules:1 always ends the think"
            ]
        );
        // Through inheritance: the trait's rule hides the kind's.
        assert_eq!(
            warnings(
                "trait t { when 2 > 1 => idle }\nkind a extends t { inherit t\n when hour > 1 => die }"
            ),
            ["t.rules:3:2: warning: never runs: the rule at t.rules:1 always ends the think"]
        );
        // One warning per rule, not one per kind that includes it.
        assert_eq!(
            warnings(
                "trait t { when true => idle\n when hour > 1 => die }\nkind a extends t { }\nkind b extends t { }"
            )
            .len(),
            1
        );
    }

    #[test]
    fn a_directory_compiles_in_sorted_file_order() {
        let dir = std::env::temp_dir().join(format!("wmc-rules-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("b.rules"), "kind bee { when 1 => become ant }").unwrap();
        std::fs::write(dir.join("a.rules"), "kind ant { }").unwrap();
        std::fs::write(dir.join("notes.txt"), "kind ignored { }").unwrap();
        let k = compile_packs(&[&dir]).unwrap();
        assert_eq!(k.names().collect::<Vec<_>>(), vec!["ant", "bee"]);
        let abs = std::fs::canonicalize(&dir).unwrap();
        assert_eq!(k.debug.packs, [abs.to_string_lossy()]);
        std::fs::write(dir.join("c.rules"), "kind ant { }").unwrap();
        let err = compile_packs(&[&dir]).unwrap_err().to_string();
        assert!(
            err.starts_with("c.rules:1:6: kind `ant` declared twice (first at a.rules:1)"),
            "{err}"
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// Packs are file lists in the order given: a later pack's kinds come
    /// after an earlier one's, may extend them and call their subs, and a
    /// name declared in two packs is an error naming both files.
    #[test]
    fn packs_merge_in_order_and_duplicates_name_both_files() {
        let root = std::env::temp_dir().join(format!("wmc-packs-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let (base, wild) = (root.join("base"), root.join("wild"));
        std::fs::create_dir_all(&base).unwrap();
        std::fs::create_dir_all(&wild).unwrap();
        std::fs::write(base.join("b.rules"), "kind hen { }\nsub rest() { idle }").unwrap();
        std::fs::write(base.join("a.rules"), "trait walker { }").unwrap();
        std::fs::write(
            wild.join("a.rules"),
            "kind wolf extends walker { when true => rest() }\nkind pup extends hen { }",
        )
        .unwrap();
        let single = root.join("lone.rules");
        std::fs::write(&single, "kind moth { }").unwrap();
        let k = compile_packs(&[&base, &wild, &single]).unwrap();
        // Pre-order: `pup` right after its parent `hen`.
        assert_eq!(
            k.names().collect::<Vec<_>>(),
            ["hen", "pup", "wolf", "moth"]
        );
        assert_eq!(k.debug.packs.len(), 3);
        assert_eq!(
            k.debug.files,
            ["base/a.rules", "base/b.rules", "wild/a.rules", "lone.rules"]
        );
        // Two packs, one name: both positions, pack-qualified.
        std::fs::write(wild.join("b.rules"), "\nkind hen { }").unwrap();
        let err = compile_packs(&[&base, &wild]).unwrap_err().to_string();
        assert!(
            err.starts_with(
                "wild/b.rules:2:6: kind `hen` declared twice (first at base/b.rules:1)"
            ),
            "{err}"
        );
        std::fs::write(wild.join("b.rules"), "const N = 1\nsub rest() { idle }").unwrap();
        let err = compile_packs(&[&base, &wild]).unwrap_err().to_string();
        assert!(
            err.contains("sub `rest` declared twice (first at base/b.rules:2)"),
            "{err}"
        );
        let err = compile_packs(&[&base, &root.join("gone")])
            .unwrap_err()
            .to_string();
        assert!(err.contains("gone"), "{err}");
        std::fs::remove_dir_all(&root).unwrap();
    }
}
