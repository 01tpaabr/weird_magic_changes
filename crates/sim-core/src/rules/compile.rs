//! The rules compiler: text -> [`Kinds`] (`docs/ACTORS.md` §5).
//!
//! Three small passes over one file: a lexer (tokens with line and column),
//! a recursive-descent parser (an AST per kind), and a code generator that
//! drives [`Asm`]. Kinds are numbered in declaration order, files in
//! sorted-name order, never directory order, so two processes agree on
//! every kind id. Every error carries `file:line:col`; the sim never runs a
//! program that did not compile.
//!
//! What is implemented is the subset the plants exercise (kind properties,
//! needs, mem, `when` rules, `if`/`else`, `choose`, `count`, `nearest ... as`,
//! `become`, `spawn`, `idle`, `die`, arithmetic, time literals). Every later
//! keyword of the grammar arrives with the creature that needs it.

use std::fmt;

use super::asm::{Asm, Label};
use super::vm::{Action, FRAME_LOCALS, OpCode, Sense, pred};
use super::{KindDef, Kinds, NeedDef};
use crate::actors::{MEM_SLOTS, NEED_SLOTS};
use crate::stage::{Feature, Ground};
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
/// by name). Kind names are global across files.
pub fn compile_files(files: &[(&str, &str)]) -> Result<Kinds> {
    let mut asts = Vec::new();
    for (name, text) in files {
        let tokens = Lexer::new(name, text).lex()?;
        let mut p = Parser {
            file: name,
            tokens,
            at: 0,
        };
        asts.extend(p.file()?);
    }
    Gen::new(&asts).generate()
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

const SYMS: [&str; 22] = [
    "=>", "==", "!=", "<=", ">=", "+=", "-=", "{", "}", "(", ")", ",", ":", "=", "<", ">", "+",
    "-", "*", "/", "%", ";",
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

#[derive(Debug, Clone)]
struct KindAst {
    file: String,
    name: String,
    line: u32,
    col: u32,
    glyph: u8,
    tags: Vec<String>,
    cadence_shift: u8,
    sight: u8,
    fuel: u32,
    food: i32,
    bite: u8,
    needs: Vec<NeedDef>,
    mems: Vec<String>,
    rules: Vec<Rule>,
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
        line: u32,
        col: u32,
    },
    And(Box<Cond>, Box<Cond>),
    Or(Box<Cond>, Box<Cond>),
    Not(Box<Cond>),
}

#[derive(Debug, Clone)]
enum Stmt {
    Assign {
        name: String,
        line: u32,
        col: u32,
        op: OpCode,
        value: Expr,
    },
    Set {
        name: String,
        line: u32,
        col: u32,
        value: Expr,
    },
    If {
        cond: Cond,
        then: Vec<Stmt>,
        els: Vec<Stmt>,
    },
    Choose(Vec<(Expr, Vec<Stmt>)>),
    Idle,
    Die,
    Become {
        kind: String,
        line: u32,
        col: u32,
    },
    Spawn {
        kind: String,
        at: Option<String>,
        line: u32,
        col: u32,
    },
}

#[derive(Debug, Clone)]
enum Pred {
    Kind(String, u32, u32),
    Ground(Ground),
    Feature(Feature),
    Free,
}

#[derive(Debug, Clone)]
enum Expr {
    Int(i32),
    Name(String, u32, u32),
    Sense(Sense),
    Bin(OpCode, Box<Expr>, Box<Expr>),
    Neg(Box<Expr>),
    Rand(Box<Expr>),
    Chance(Box<Expr>),
    Count(Pred, Box<Expr>),
    /// `min`, `max`, `abs`, `sign`, `clamp`: the opcode and its arguments.
    Fn(OpCode, Vec<Expr>),
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
        _ => return None,
    })
}

const KEYWORDS: [&str; 26] = [
    "kind", "glyph", "tags", "cadence", "sight", "fuel", "food", "bite", "need", "max", "decay",
    "vital", "mem", "when", "if", "else", "choose", "and", "or", "not", "nearest", "count",
    "within", "as", "true", "false",
];

impl Parser<'_> {
    fn peek(&self) -> &Tok {
        &self.tokens[self.at].tok
    }

    fn pos(&self) -> (u32, u32) {
        let t = &self.tokens[self.at];
        (t.line, t.col)
    }

    fn err_at(&self, (line, col): (u32, u32), msg: impl Into<String>) -> CompileError {
        CompileError {
            file: self.file.to_string(),
            line,
            col,
            msg: msg.into(),
        }
    }

    fn err(&self, msg: impl Into<String>) -> CompileError {
        self.err_at(self.pos(), msg)
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
    fn ident(&mut self, what: &str) -> Result<(String, u32, u32)> {
        let (line, col) = self.pos();
        match self.peek().clone() {
            Tok::Name(n) if !KEYWORDS.contains(&n.as_str()) && sense_named(&n).is_none() => {
                self.bump();
                Ok((n, line, col))
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

    fn file(&mut self) -> Result<Vec<KindAst>> {
        let mut kinds = Vec::new();
        while *self.peek() != Tok::Eof {
            if !self.is_kw("kind") {
                return Err(self.err(format!("expected `kind`, found {}", self.describe())));
            }
            kinds.push(self.kind()?);
        }
        Ok(kinds)
    }

    fn kind(&mut self) -> Result<KindAst> {
        self.expect_kw("kind")?;
        let (name, line, col) = self.ident("kind name")?;
        self.expect_sym("{")?;
        let mut k = KindAst {
            file: self.file.to_string(),
            name,
            line,
            col,
            glyph: b'?',
            tags: Vec::new(),
            cadence_shift: 3,
            sight: 4,
            fuel: 512,
            food: 0,
            bite: 1,
            needs: Vec::new(),
            mems: Vec::new(),
            rules: Vec::new(),
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
                    _ => return Err(self.err_at(p, "glyph takes one printable ASCII character")),
                }
            } else if self.eat_kw("tags") {
                while let Tok::Name(n) = self.peek().clone() {
                    if KEYWORDS.contains(&n.as_str()) {
                        break;
                    }
                    self.bump();
                    k.tags.push(n);
                }
            } else if self.eat_kw("cadence") {
                let c = self.int("a cadence")?;
                if c < 1 || !(c as u32).is_power_of_two() {
                    return Err(self.err_at(p, "cadence must be a power of two (1, 2, 4, ...)"));
                }
                k.cadence_shift = c.trailing_zeros() as u8;
            } else if self.eat_kw("sight") {
                let s = self.int("a sight radius")?;
                if !(0..=16).contains(&s) {
                    return Err(self.err_at(p, "sight is 0 to 16 cells"));
                }
                k.sight = s as u8;
            } else if self.eat_kw("fuel") {
                let f = self.int("a fuel budget")?;
                if !(1..=4096).contains(&f) {
                    return Err(self.err_at(p, "fuel is 1 to 4096 ops per think"));
                }
                k.fuel = f as u32;
            } else if self.eat_kw("food") {
                k.food = self.int_or_time("a food value")?;
            } else if self.eat_kw("bite") {
                let b = self.int("a bite")?;
                if !(0..=255).contains(&b) {
                    return Err(self.err_at(p, "bite is 0 to 255"));
                }
                k.bite = b as u8;
            } else if self.eat_kw("need") {
                let (name, ..) = self.ident("need name")?;
                self.expect_kw("max")?;
                let max = self.int_or_time("the need's maximum")?;
                if max < 1 {
                    return Err(self.err_at(p, "a need's max is at least 1"));
                }
                let mut decays = true;
                if self.eat_kw("decay") {
                    match self.int("0 (points) or 1 (per tick)")? {
                        0 => decays = false,
                        1 => decays = true,
                        _ => return Err(self.err_at(p, "decay is 0 (points) or 1 (per tick)")),
                    }
                }
                let vital = self.eat_kw("vital");
                if k.needs.iter().any(|n| n.name == name) {
                    return Err(self.err_at(p, format!("need `{name}` declared twice")));
                }
                if k.needs.len() == NEED_SLOTS {
                    return Err(self.err_at(p, format!("at most {NEED_SLOTS} needs per kind")));
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
                        return Err(self.err_at(p, format!("`{name}` declared twice")));
                    }
                    if k.mems.len() == MEM_SLOTS {
                        return Err(
                            self.err_at(p, format!("at most {MEM_SLOTS} mem slots per kind"))
                        );
                    }
                    k.mems.push(name);
                    if !self.eat_sym(",") {
                        break;
                    }
                }
            } else if self.is_kw("when") {
                break;
            } else {
                return Err(self.err(format!(
                    "expected a declaration or `when`, found {}",
                    self.describe()
                )));
            }
        }
        while self.eat_kw("when") {
            let cond = self.cond()?;
            self.expect_sym("=>")?;
            let body = self.body()?;
            k.rules.push(Rule { cond, body });
        }
        if !self.is_sym("}") {
            return Err(self.err(format!(
                "expected `when` or `}}`, found {}",
                self.describe()
            )));
        }
        self.expect_sym("}")?;
        Ok(k)
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
        let (line, col) = self.pos();
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
                return Err(self.err_at((line, col), "choose needs at least one arm"));
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
            let (kind, line, col) = self.ident("kind name")?;
            return Ok(Stmt::Become { kind, line, col });
        }
        if self.eat_kw("spawn") {
            let (kind, line, col) = self.ident("kind name")?;
            self.expect_kw("at")?;
            let at = if self.eat_kw("here") {
                None
            } else {
                Some(self.ident("a bound target")?.0)
            };
            return Ok(Stmt::Spawn {
                kind,
                at,
                line,
                col,
            });
        }
        // Assignment.
        let (name, line, col) = self.ident("statement")?;
        if self.eat_sym("=") {
            let value = self.expr()?;
            return Ok(Stmt::Set {
                name,
                line,
                col,
                value,
            });
        }
        for (sym, op) in [("+=", OpCode::Add), ("-=", OpCode::Sub)] {
            if self.eat_sym(sym) {
                let value = self.expr()?;
                return Ok(Stmt::Assign {
                    name,
                    line,
                    col,
                    op,
                    value,
                });
            }
        }
        Err(self.err(format!(
            "expected `=`, `+=` or `-=` after `{name}`, found {}",
            self.describe()
        )))
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
        let (line, col) = self.pos();
        if self.eat_kw("nearest") {
            let pred = self.pred()?;
            self.expect_kw("within")?;
            let r = self.additive()?; // a radius, never a comparison
            self.expect_kw("as")?;
            let (bind, ..) = self.ident("binding name")?;
            return Ok(Cond::Nearest {
                pred,
                r,
                bind,
                line,
                col,
            });
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
        let (line, col) = self.pos();
        if self.eat_kw("free") {
            return Ok(Pred::Free);
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
        let (name, ..) = self.ident("predicate (a kind, water, soil, rock or free)")?;
        Ok(Pred::Kind(name, line, col))
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
        let (line, col) = self.pos();
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
                    "min" | "max" | "abs" | "sign" | "clamp" => {
                        let (op, arity) = match n.as_str() {
                            "min" => (OpCode::Min, 2),
                            "max" => (OpCode::Max, 2),
                            "abs" => (OpCode::Abs, 1),
                            "sign" => (OpCode::Sign, 1),
                            _ => (OpCode::Clamp, 3),
                        };
                        self.expect_sym("(")?;
                        let mut args = vec![self.expr()?];
                        while self.eat_sym(",") {
                            args.push(self.expr()?);
                        }
                        self.expect_sym(")")?;
                        if args.len() != arity {
                            return Err(
                                self.err_at((line, col), format!("`{n}` takes {arity} arguments"))
                            );
                        }
                        Ok(Expr::Fn(op, args))
                    }
                    _ if KEYWORDS.contains(&n.as_str()) => {
                        Err(self.err_at((line, col), format!("unexpected `{n}` in an expression")))
                    }
                    _ => Ok(Expr::Name(n, line, col)),
                }
            }
            _ => {
                self.at -= 1;
                Err(self.err(format!("expected an expression, found {}", self.describe())))
            }
        }
    }
}

// ---- code generation ------------------------------------------------------------------

struct Gen<'a> {
    asts: &'a [KindAst],
    asm: Asm,
    consts: Vec<i32>,
    /// Current kind, its bound locals (name, slot), next free local.
    kind: usize,
    locals: Vec<(String, u8)>,
    next_local: u8,
}

impl<'a> Gen<'a> {
    fn new(asts: &'a [KindAst]) -> Self {
        Self {
            asts,
            asm: Asm::new(),
            consts: Vec::new(),
            kind: 0,
            locals: Vec::new(),
            next_local: 0,
        }
    }

    fn err(&self, line: u32, col: u32, msg: impl Into<String>) -> CompileError {
        CompileError {
            file: self.asts[self.kind].file.clone(),
            line,
            col,
            msg: msg.into(),
        }
    }

    fn kind_id(&self, name: &str) -> Option<u16> {
        self.asts
            .iter()
            .position(|k| k.name == name)
            .map(|i| i as u16)
    }

    fn generate(mut self) -> Result<Kinds> {
        // Duplicate kind names across files.
        for (i, k) in self.asts.iter().enumerate() {
            if self.asts[..i].iter().any(|o| o.name == k.name) {
                self.kind = i;
                return Err(self.err(k.line, k.col, format!("kind `{}` declared twice", k.name)));
            }
        }
        let mut defs = Vec::with_capacity(self.asts.len());
        for i in 0..self.asts.len() {
            self.kind = i;
            let entry = self.asm.here();
            let k = &self.asts[i];
            for rule in &k.rules {
                self.locals.clear();
                self.next_local = 0;
                let next = self.asm.label();
                self.cond(&rule.cond, next, true)?;
                self.stmts(&rule.body)?;
                self.asm.end_rule().bind(next);
            }
            self.asm.halt();
            defs.push(KindDef {
                id: i as u16,
                name: k.name.clone(),
                glyph: k.glyph,
                tags: 0,
                cadence_shift: k.cadence_shift,
                sight: k.sight,
                fuel: k.fuel,
                food: k.food,
                bite: k.bite,
                needs: k.needs.clone(),
                mems: k.mems.clone(),
                states: 1,
                entry,
            });
        }
        let code = self.asm.finish();
        Ok(Kinds::from_parts(defs, code, self.consts, Vec::new()))
    }

    fn push_int(&mut self, v: i32) {
        if let Ok(imm) = i16::try_from(v) {
            self.asm.push(i32::from(imm));
        } else {
            let idx = match self.consts.iter().position(|&c| c == v) {
                Some(i) => i,
                None => {
                    self.consts.push(v);
                    self.consts.len() - 1
                }
            };
            self.asm
                .push_k(u16::try_from(idx).expect("constant pool fits u16"));
        }
    }

    fn alloc_local(&mut self, line: u32, col: u32, n: u8) -> Result<u8> {
        let slot = self.next_local;
        if usize::from(slot) + usize::from(n) > FRAME_LOCALS {
            return Err(self.err(
                line,
                col,
                format!("too many bindings in one rule (max {FRAME_LOCALS} locals)"),
            ));
        }
        self.next_local += n;
        Ok(slot)
    }

    /// Emit `cond`; on false, jump to `on_false`. `top` is true while the
    /// condition is a top-level conjunct, where `as v` bindings are allowed.
    fn cond(&mut self, c: &Cond, on_false: Label, top: bool) -> Result<()> {
        match c {
            Cond::Expr(e) => {
                self.expr(e)?;
                self.asm.jz(on_false);
            }
            Cond::Nearest {
                pred,
                r,
                bind,
                line,
                col,
            } => {
                if !top {
                    return Err(self.err(
                        *line,
                        *col,
                        "`nearest ... as` must be a top-level conjunct (not under `or` or `not`)",
                    ));
                }
                if self.locals.iter().any(|(n, _)| n == bind) {
                    return Err(self.err(*line, *col, format!("`{bind}` is already bound")));
                }
                let slot = self.alloc_local(*line, *col, 2)?;
                self.pred(pred)?;
                self.expr(r)?;
                self.asm.nearest(slot).jz(on_false);
                self.locals.push((bind.clone(), slot));
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
            Pred::Ground(g) => pred::ground(*g as u8),
            Pred::Feature(f) => pred::feature(*f as u8),
            Pred::Kind(name, line, col) => i32::from(
                self.kind_id(name)
                    .ok_or_else(|| self.err(*line, *col, format!("unknown kind `{name}`")))?,
            ),
        };
        self.push_int(v);
        Ok(())
    }

    fn expr(&mut self, e: &Expr) -> Result<()> {
        match e {
            Expr::Int(v) => self.push_int(*v),
            Expr::Sense(s) => {
                self.asm.sense(*s);
            }
            Expr::Name(n, line, col) => {
                if let Some(&(_, slot)) = self.locals.iter().rev().find(|(x, _)| x == n) {
                    self.asm.load(slot);
                } else if let Some(i) = self.need_slot(n) {
                    self.asm.need(i);
                } else if let Some(i) = self.mem_slot(n) {
                    self.asm.mem(i);
                } else {
                    return Err(self.err(
                        *line,
                        *col,
                        format!(
                            "unknown name `{n}` (not a need, mem slot or binding of this kind)"
                        ),
                    ));
                }
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
            Expr::Fn(op, args) => {
                for a in args {
                    self.expr(a)?;
                }
                self.asm.op(*op);
            }
        }
        Ok(())
    }

    fn need_slot(&self, n: &str) -> Option<u8> {
        self.asts[self.kind]
            .needs
            .iter()
            .position(|d| d.name == n)
            .map(|i| i as u8)
    }

    fn mem_slot(&self, n: &str) -> Option<u8> {
        self.asts[self.kind]
            .mems
            .iter()
            .position(|m| m == n)
            .map(|i| i as u8)
    }

    fn stmts(&mut self, body: &[Stmt]) -> Result<()> {
        for s in body {
            self.stmt(s)?;
        }
        Ok(())
    }

    fn store(&mut self, name: &str, line: u32, col: u32) -> Result<()> {
        if let Some(&(_, slot)) = self.locals.iter().rev().find(|(x, _)| x == name) {
            self.asm.store(slot);
        } else if let Some(i) = self.need_slot(name) {
            self.asm.set_need(i);
        } else if let Some(i) = self.mem_slot(name) {
            self.asm.set_mem(i);
        } else {
            return Err(self.err(
                line,
                col,
                format!("cannot assign to `{name}`: not a need or mem slot of this kind"),
            ));
        }
        Ok(())
    }

    fn stmt(&mut self, s: &Stmt) -> Result<()> {
        match s {
            Stmt::Set {
                name,
                line,
                col,
                value,
            } => {
                self.expr(value)?;
                self.store(name, *line, *col)?;
            }
            Stmt::Assign {
                name,
                line,
                col,
                op,
                value,
            } => {
                self.expr(&Expr::Name(name.clone(), *line, *col))?;
                self.expr(value)?;
                self.asm.op(*op);
                self.store(name, *line, *col)?;
            }
            Stmt::If { cond, then, els } => {
                let saved = (self.locals.len(), self.next_local);
                let no = self.asm.label();
                let end = self.asm.label();
                self.cond(cond, no, true)?;
                self.stmts(then)?;
                self.locals.truncate(saved.0);
                self.next_local = saved.1;
                if els.is_empty() {
                    self.asm.bind(no);
                } else {
                    self.asm.jmp(end).bind(no);
                    self.stmts(els)?;
                    self.asm.bind(end);
                }
            }
            Stmt::Choose(arms) => self.choose(arms)?,
            Stmt::Idle => {
                self.asm.act(Action::Idle);
            }
            Stmt::Die => {
                self.asm.act(Action::Die);
            }
            Stmt::Become { kind, line, col } => {
                let id = self
                    .kind_id(kind)
                    .ok_or_else(|| self.err(*line, *col, format!("unknown kind `{kind}`")))?;
                self.push_int(i32::from(id));
                self.asm.act(Action::Become);
            }
            Stmt::Spawn {
                kind,
                at,
                line,
                col,
            } => {
                let id = self
                    .kind_id(kind)
                    .ok_or_else(|| self.err(*line, *col, format!("unknown kind `{kind}`")))?;
                self.push_int(i32::from(id));
                match at {
                    None => {
                        self.asm.push(0).push(0);
                    }
                    Some(name) => {
                        let slot = self
                            .locals
                            .iter()
                            .rev()
                            .find(|(x, _)| x == name)
                            .map(|&(_, s)| s)
                            .ok_or_else(|| {
                                self.err(*line, *col, format!("`{name}` is not a bound target"))
                            })?;
                        self.asm.load(slot).load(slot + 1);
                    }
                }
                self.asm.act(Action::Spawn);
            }
        }
        Ok(())
    }

    /// `choose { w1: b1 ... wn: bn }`: weights into locals, one draw in
    /// `0..total`, the first arm whose cumulative weight exceeds it runs.
    /// A total of zero runs nothing.
    fn choose(&mut self, arms: &[(Expr, Vec<Stmt>)]) -> Result<()> {
        let saved = (self.locals.len(), self.next_local);
        let n = arms.len();
        let base = self.alloc_local(0, 0, (n + 1) as u8)?;
        let draw = base + n as u8;
        // total = sum(max(w_i, 0)), each weight stored.
        self.asm.push(0);
        for (i, (w, _)) in arms.iter().enumerate() {
            self.expr(w)?;
            self.asm.push(0).op(OpCode::Max).store(base + i as u8);
            self.asm.load(base + i as u8).op(OpCode::Add);
        }
        self.asm.op(OpCode::Rand).store(draw);
        let end = self.asm.label();
        for (i, (_, body)) in arms.iter().enumerate() {
            let skip = self.asm.label();
            // if draw < w_i { body; goto end } else { draw -= w_i }
            self.asm
                .load(draw)
                .load(base + i as u8)
                .op(OpCode::Lt)
                .jz(skip);
            self.stmts(body)?;
            self.asm.jmp(end).bind(skip);
            if i + 1 < n {
                self.asm
                    .load(draw)
                    .load(base + i as u8)
                    .op(OpCode::Sub)
                    .store(draw);
            }
        }
        self.asm.bind(end);
        self.locals.truncate(saved.0);
        self.next_local = saved.1;
        Ok(())
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
        let text = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../rules/plants.rules"
        ))
        .unwrap();
        let compiled = compile("plants.rules", &text).unwrap_or_else(|e| panic!("{e}"));
        let expected = crate::rules::builtin::hand_assembled();
        let ops = |k: &Kinds| {
            k.code
                .iter()
                .map(|o| format!("{:?}", o))
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
        let toks = Lexer::new("t", "kind x { # c\n glyph \"T\" 6h 30min 2d 7 => >= += }")
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
        assert!(compile_err("kind a { when 1 => spawn a at c }").contains("not a bound target"));
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
                .contains("expected `when` or `}`")
        );
        assert!(compile_err("kind a { when 1 => { idle").contains("unclosed block"));
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
        assert!(compile_err("kind a { when min(1) > 0 => idle }").contains("takes 2 arguments"));
        assert!(compile_err("kind a { need n max 1h mem n }").contains("declared twice"));
        let many: String = (0..5).map(|i| format!("need n{i} max 1h ")).collect();
        assert!(compile_err(&format!("kind a {{ {many} }}")).contains("at most 4 needs"));
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
        // Rule 1: (m > 1 or m < -1) and not (m == 0)
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
        ); // or: a false -> try b
        expect(&mut i, &[OpCode::Mem, OpCode::Push, OpCode::Lt, OpCode::Jz]); // b false -> rule fails
        expect(
            &mut i,
            &[
                OpCode::Mem,
                OpCode::Push,
                OpCode::Eq,
                OpCode::Jz,
                OpCode::Jmp,
            ],
        ); // not
        expect(&mut i, &[OpCode::Push, OpCode::SetMem, OpCode::EndRule]);
        // Rule 2.
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
        // Rule 3: if / else if / else.
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
    fn choose_draws_once_and_large_constants_use_the_pool() {
        let k = compile_ok(
            "kind a { mem m\n when 1 => choose { 3: m = 1  2: m = 2 }\n when m > 100000 => m = 3d }",
        );
        assert_eq!(k.consts, vec![100_000, days(3) as i32]);
        let rand = k.code.iter().filter(|o| o.code == OpCode::Rand).count();
        assert_eq!(rand, 1);
        // Run it a few hundred times on different uids: both arms get picked,
        // about 3:2, and never anything else.
        use crate::actors::ChunkActors;
        use crate::rules::vm::{self, Ctx, Halo};
        use crate::stage::{ChunkCells, Pos};
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
        };
        let mut counts = [0; 3];
        for uid in 0..500u64 {
            let mut mind = crate::actors::ActorMind::zeroed();
            let ctx = Ctx {
                halo: &halo,
                kind: &k.defs[0],
                cell: 100,
                pos: Pos::new(36, 1),
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
