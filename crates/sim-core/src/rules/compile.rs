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
use super::vm::{SEARCH_DEFAULT_LIMIT, SEARCH_STATE};
use super::{
    DEFAULT_COLOR, DebugInfo, Diagnostic, FieldDef, FieldInit, GeneDef, KindDef, Kinds, LayerDef,
    Level, MAX_LAYERS, MaterialDef, Materials, NeedDef, Players, Policy, RuleInfo, RuleShape, Term,
};
use crate::actors::{MEM_SLOTS, NEED_SLOTS};
use crate::stage::{Feature, Ground, MAX_FEATURES, MAX_GROUNDS};
use crate::time::{days, hours, minutes};

mod law;
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
    /// `file:line:col: message`; `file: message` for an error about no
    /// place in a file (line 0: a `--set`, a rule set too large).
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.line == 0 {
            return write!(f, "{}: {}", self.file, self.msg);
        }
        write!(f, "{}:{}:{}: {}", self.file, self.line, self.col, self.msg)
    }
}

impl std::error::Error for CompileError {}

type Result<T> = std::result::Result<T, CompileError>;

/// Compile one file's text.
pub fn compile(file: &str, text: &str) -> Result<Kinds> {
    compile_files(&[(file, text)])
}

/// A `const` set from outside the rules (`wmc --set NAME=V`): it replaces
/// the value the rules declare, and constants declared after it fold with
/// the new one.
pub type ConstSet = (String, i32);

/// Compile several files as one rule set, in the order given (callers sort
/// by name). Kind and sub names are global across files.
pub fn compile_files(files: &[(&str, &str)]) -> Result<Kinds> {
    compile_files_with(files, &[])
}

/// [`compile_files`] with some constants set from outside: each must name a
/// `const` the files declare.
pub fn compile_files_with(files: &[(&str, &str)], consts: &[ConstSet]) -> Result<Kinds> {
    let owned: Vec<(&str, &str, Option<&str>)> = files.iter().map(|&(n, t)| (n, t, None)).collect();
    compile_owned(&owned, consts)
}

/// A pack given as `NAME=PATH` is player NAME's (`docs/WORLD.md` §12): its
/// name and the path; any other, the arena's.
pub fn player_of(pack: &str) -> (Option<&str>, &str) {
    match pack.split_once('=') {
        Some((n, path))
            if n.starts_with(|c: char| c.is_ascii_lowercase())
                && n.chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_') =>
        {
            (Some(n), path)
        }
        _ => (None, pack),
    }
}

/// Players in a rule set: each has an index below [`Players::NONE`].
const MAX_PLAYERS: usize = Players::NONE as usize - 1;

/// Words a player's item can't take: renamed into the player's namespace,
/// they would stop meaning what the language reads them as.
const NOT_PLAYER_NAMES: [&str; 52] = [
    "group",
    "size",
    "food",
    "health",
    "water",
    "soil",
    "rock",
    "bare",
    "target",
    "pred",
    "dx",
    "dy",
    "conserved",
    "fixed",
    "private",
    "init",
    "noise",
    "scale",
    "outside",
    "every",
    "relief",
    "ground",
    "feature",
    "layer",
    "flies",
    "walkable",
    "drink",
    "blocks",
    "opaque",
    "budget",
    "rows",
    "namespace",
    "goal",
    "guess",
    "limit",
    "cost",
    "via",
    "steps",
    "into",
    "reach",
    "climb",
    "along",
    "over",
    "eye",
    "on",
    "sum",
    "avg",
    "count",
    "min",
    "max",
    "glyph",
    "color",
];

/// [`compile_files_with`] for files that may belong to players (a file's
/// owner, `None` for the arena). Each player's top-level names (kinds,
/// traits, subs, consts, fields) are renamed `PLAYER.NAME` across its own
/// files, so two players may both have an `ant` and neither can name the
/// other's; it may declare kinds, traits, subs, consts and `private` fields
/// only, and no name the arena has. A world without players is as before.
pub fn compile_owned(files: &[(&str, &str, Option<&str>)], consts: &[ConstSet]) -> Result<Kinds> {
    if let Some((name, _, _)) = files.get(MAX_FILES) {
        return Err(CompileError {
            file: name.to_string(),
            line: 1,
            col: 1,
            msg: format!("at most {MAX_FILES} rules files in a rule set"),
        });
    }
    let parse = |file: &str, tokens: Vec<Token>, items: &mut Items| -> Result<()> {
        Parser {
            file,
            tokens,
            at: 0,
            depth: 0,
            deepest: 0,
            top: None,
            conjuncts: Vec::new(),
            law: false,
        }
        .file(items)
    };
    let mut lexed = Vec::with_capacity(files.len());
    for &(name, text, owner) in files {
        lexed.push((name, Lexer::new(name, text).lex()?, owner));
    }
    let spaces = namespaces(&mut lexed, parse)?;
    let mut items = Items::default();
    for (name, tokens, _) in lexed.iter().filter(|(_, _, o)| o.is_none()) {
        parse(name, tokens.clone(), &mut items)?;
    }
    if let Some((n, at)) = top_names(&items).find(|(n, _)| spaces.iter().any(|(s, _)| s == *n)) {
        return Err(pos_err(
            at,
            format!("`{n}` is a namespace: an item takes another name"),
        ));
    }
    if let Some(f) = items.fields.iter().find(|f| f.private) {
        return Err(pos_err(
            &f.at,
            "only a player's pack declares a `private` field",
        ));
    }
    if let Some(b) = items.budgets.get(1) {
        return Err(pos_err(&b.at, "one `budget` in a rule set"));
    }
    let mut players: Vec<&str> = Vec::new();
    for &(_, _, o) in &lexed {
        if let Some(p) = o
            && !players.contains(&p)
        {
            players.push(p);
        }
    }
    if players.len() > MAX_PLAYERS {
        return Err(CompileError {
            file: "--rules".into(),
            line: 0,
            col: 0,
            msg: format!(
                "{} players: a rule set has at most {MAX_PLAYERS}",
                players.len()
            ),
        });
    }
    if let Some(p) = players.iter().find(|p| spaces.iter().any(|(s, _)| s == *p)) {
        return Err(CompileError {
            file: "--rules".into(),
            line: 0,
            col: 0,
            msg: format!("`{p}` is both a player and a namespace"),
        });
    }
    let player_list: Vec<String> = players.iter().map(|p| p.to_string()).collect();
    for player in players {
        let mine = || lexed.iter().filter(move |(_, _, o)| *o == Some(player));
        let mut own = Items::default();
        for (name, tokens, _) in mine() {
            parse(name, tokens.clone(), &mut own)?;
        }
        let names = player_names(player, &own, &items)?;
        for (name, tokens, _) in mine() {
            let mut tokens = tokens.clone();
            for t in &mut tokens {
                if let Tok::Name(n) = &t.tok
                    && names.contains(n)
                {
                    t.tok = Tok::Name(format!("{player}.{n}"));
                }
            }
            parse(name, tokens, &mut items)?;
        }
    }
    let pairs: Vec<(&str, &str)> = files.iter().map(|&(n, t, _)| (n, t)).collect();
    let mut g = Gen::new(&items, &pairs, consts);
    g.player_list = player_list;
    g.generate()
}

/// The array an element slot's name belongs to: `route` for `route[3]`.
fn array_of(slot: &str) -> Option<&str> {
    slot.strip_suffix(']')?.split_once('[').map(|(n, _)| n)
}

/// Every top-level name `items` declare, with where.
fn top_names(items: &Items) -> impl Iterator<Item = (&str, &Pos)> {
    items
        .items
        .iter()
        .map(|i| (i.name.as_str(), &i.at))
        .chain(items.subs.iter().map(|s| (s.name.as_str(), &s.at)))
        .chain(items.consts.iter().map(|c| (c.name.as_str(), &c.at)))
        .chain(items.fields.iter().map(|f| (f.name.as_str(), &f.at)))
        .chain(items.materials.iter().map(|m| (m.name.as_str(), &m.at)))
        .chain(items.layers.iter().map(|(l, at, _)| (l.name.as_str(), at)))
}

/// Namespaces (`docs/RULES.md` §15): an arena file that starts `namespace
/// NAME` puts its top-level names in NAME, so two packs of different
/// authors don't collide. Inside the namespace's files a name stays plain;
/// every file names it `NAME.ITEM`. Done on tokens, before parsing: the
/// namespace's names become `NAME.ITEM` in its own files, and `NAME . ITEM`
/// becomes that one name everywhere. Tags and the words the language reads
/// (`water`, `food`, ...) stay global. Returns each namespace and its names.
fn namespaces(
    lexed: &mut [(&str, Vec<Token>, Option<&str>)],
    parse: impl Fn(&str, Vec<Token>, &mut Items) -> Result<()>,
) -> Result<Vec<(String, Vec<String>)>> {
    let mut spaces: Vec<(String, Vec<String>)> = Vec::new();
    let mut space_of: Vec<Option<usize>> = Vec::with_capacity(lexed.len());
    for (name, tokens, owner) in lexed.iter_mut() {
        let Some(Token {
            tok: Tok::Name(kw),
            line,
            col,
        }) = tokens.first()
        else {
            space_of.push(None);
            continue;
        };
        if kw != "namespace" {
            space_of.push(None);
            continue;
        }
        let at = Pos {
            file: name.to_string(),
            line: *line,
            col: *col,
        };
        let Some(Token {
            tok: Tok::Name(ns), ..
        }) = tokens.get(1)
        else {
            return Err(pos_err(&at, "expected a name after `namespace`"));
        };
        if owner.is_some() {
            return Err(pos_err(
                &at,
                "a player's names are its own already: its pack declares no namespace",
            ));
        }
        if ns.contains('.') || is_reserved(ns) || NOT_PLAYER_NAMES.contains(&ns.as_str()) {
            return Err(pos_err(&at, format!("`{ns}` can't name a namespace")));
        }
        let i = match spaces.iter().position(|(s, _)| s == ns) {
            Some(i) => i,
            None => {
                spaces.push((ns.clone(), Vec::new()));
                spaces.len() - 1
            }
        };
        tokens.drain(..2);
        if let Some(t) = tokens.first()
            && t.tok == Tok::Name("namespace".into())
        {
            return Err(pos_err(
                &Pos {
                    file: name.to_string(),
                    line: t.line,
                    col: t.col,
                },
                "one `namespace` line a file",
            ));
        }
        space_of.push(Some(i));
    }
    if spaces.is_empty() {
        return Ok(spaces);
    }
    for (i, (_, names)) in spaces.iter_mut().enumerate() {
        let mut own = Items::default();
        for ((name, tokens, _), s) in lexed.iter().zip(&space_of) {
            if *s == Some(i) {
                parse(name, tokens.clone(), &mut own)?;
            }
        }
        // The engine's soil, water and rock redeclared stay the engine's.
        let engine = |n: &str| ["soil", "water", "rock"].contains(&n);
        for (n, at) in top_names(&own) {
            let material = own.materials.iter().any(|m| m.name == n);
            if material && engine(n) {
                continue;
            }
            if NOT_PLAYER_NAMES.contains(&n) {
                return Err(pos_err(
                    at,
                    format!(
                        "`{n}` is a word the language reads: a namespace's item takes another name"
                    ),
                ));
            }
            if !names.iter().any(|m| m == n) {
                names.push(n.to_string());
            }
        }
    }
    for ((_, tokens, _), s) in lexed.iter_mut().zip(&space_of) {
        let mut out = Vec::with_capacity(tokens.len());
        let mut k = 0;
        while k < tokens.len() {
            let t = &tokens[k];
            if let (Tok::Name(a), Some(Tok::Sym(".")), Some(Tok::Name(b))) = (
                &t.tok,
                tokens.get(k + 1).map(|t| &t.tok),
                tokens.get(k + 2).map(|t| &t.tok),
            ) && spaces
                .iter()
                .any(|(sp, names)| sp == a && names.contains(b))
            {
                out.push(Token {
                    tok: Tok::Name(format!("{a}.{b}")),
                    ..t.clone()
                });
                k += 3;
                continue;
            }
            let mut t = t.clone();
            if let (Some(i), Tok::Name(n)) = (s, &t.tok) {
                let (sp, names) = &spaces[*i];
                if names.contains(n) {
                    t.tok = Tok::Name(format!("{sp}.{n}"));
                }
            }
            out.push(t);
            k += 1;
        }
        *tokens = out;
    }
    Ok(spaces)
}

fn pos_err(at: &Pos, msg: impl Into<String>) -> CompileError {
    CompileError {
        file: at.file.clone(),
        line: at.line,
        col: at.col,
        msg: msg.into(),
    }
}

/// What player `player`'s files (`own`, parsed apart) declare at the top
/// level, after checking it keeps to a player's part: no ground, feature,
/// layer or shared field, no name the arena (`arena`) has or the language
/// reads.
fn player_names(player: &str, own: &Items, arena: &Items) -> Result<Vec<String>> {
    if let Some(m) = own.materials.first() {
        let what = if m.ground { "ground" } else { "feature" };
        return Err(pos_err(
            &m.at,
            format!(
                "`{}`: a player's pack declares no {what}: the arena shapes the world",
                m.name
            ),
        ));
    }
    if let Some((l, at, _)) = own.layers.first() {
        return Err(pos_err(
            at,
            format!(
                "`{}`: a player's pack declares no layer: the arena shapes the world",
                l.name
            ),
        ));
    }
    if let Some(b) = own.budgets.first() {
        return Err(pos_err(
            &b.at,
            "a player's pack declares no budget: the arena sets it",
        ));
    }
    if let Some(f) = own.fields.iter().find(|f| !f.private) {
        return Err(pos_err(
            &f.at,
            format!(
                "`{}`: a player's field is `private` (its own); the arena declares the shared ones",
                f.name
            ),
        ));
    }
    let mut arena_names: Vec<&str> = vec!["soil", "water", "rock"];
    for i in &arena.items {
        arena_names.push(&i.name);
        arena_names.extend(i.members.iter().map(|s| s.name.as_str()));
        arena_names.extend(i.decls.tags.iter().map(String::as_str));
    }
    arena_names.extend(arena.subs.iter().map(|s| s.name.as_str()));
    arena_names.extend(arena.consts.iter().map(|c| c.name.as_str()));
    arena_names.extend(arena.fields.iter().map(|f| f.name.as_str()));
    arena_names.extend(arena.materials.iter().map(|m| m.name.as_str()));
    arena_names.extend(arena.layers.iter().map(|(l, ..)| l.name.as_str()));
    let declared = own
        .items
        .iter()
        .map(|i| (&i.name, &i.at))
        .chain(own.subs.iter().map(|s| (&s.name, &s.at)))
        .chain(own.consts.iter().map(|c| (&c.name, &c.at)))
        .chain(own.fields.iter().map(|f| (&f.name, &f.at)));
    let mut names = Vec::new();
    for (name, at) in declared {
        if arena_names.contains(&name.as_str()) {
            return Err(pos_err(
                at,
                format!("`{name}` is the arena's: player `{player}` names its own otherwise"),
            ));
        }
        if NOT_PLAYER_NAMES.contains(&name.as_str()) {
            return Err(pos_err(
                at,
                format!(
                    "`{name}` is a word the language reads: a player's item takes another name"
                ),
            ));
        }
        if !names.contains(name) {
            names.push(name.clone());
        }
    }
    Ok(names)
}

/// Compile rule packs as one rule set. A pack is a directory (its `*.rules`
/// files, in sorted file-name order; a directory with none is an error) or
/// a single file; packs go in the order given, with one set of names
/// across all of them (a name declared in two packs is an error naming
/// both), but for a file that starts `namespace NAME` and a player's pack
/// (`NAME=PATH`): their names are `NAME.ITEM`. A pack given twice loads
/// once.
/// With more than one pack, a directory's file is named `pack/file.rules`
/// in positions, `pack` being the directory's own name; two files that
/// would share a name take parent directories until they don't. The
/// packs' absolute paths go in `debug.packs`: a save remembers them.
pub fn compile_packs(
    packs: &[&std::path::Path],
) -> std::result::Result<Kinds, Box<dyn std::error::Error>> {
    compile_packs_with(packs, &[])
}

/// [`compile_packs`] with some constants set from outside
/// ([`compile_files_with`]).
pub fn compile_packs_with(
    packs: &[&std::path::Path],
    consts: &[ConstSet],
) -> std::result::Result<Kinds, Box<dyn std::error::Error>> {
    // Each pack once, where first given: a scenario's `rules` line and the
    // same `--rules` are one pack. `NAME=PATH` is player NAME's.
    let mut full_paths: Vec<(&std::path::Path, std::path::PathBuf, Option<&str>)> = Vec::new();
    for pack in packs {
        let (owner, path) = match pack.to_str() {
            Some(s) => player_of(s),
            None => (None, ""),
        };
        let pack = if path.is_empty() {
            *pack
        } else {
            std::path::Path::new(path)
        };
        let full = std::fs::canonicalize(pack).map_err(|e| format!("{}: {e}", pack.display()))?;
        if !full_paths.iter().any(|(_, f, _)| *f == full) {
            full_paths.push((pack, full, owner));
        }
    }
    // Every file once, where first loaded, with how many trailing path
    // components label it, and its owner.
    let mut files: Vec<(std::path::PathBuf, usize, Option<&str>)> = Vec::new();
    for (pack, full, owner) in &full_paths {
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
                if !files.iter().any(|(q, _, _)| *q == p) {
                    files.push((p, k, *owner));
                }
            }
        } else if !files.iter().any(|(q, _, _)| q == full) {
            files.push((full.clone(), 1, *owner));
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
        let labels: Vec<String> = files.iter().map(|(p, k, _)| label(p, *k)).collect();
        let mut grew = false;
        for (i, (p, k, _)) in files.iter_mut().enumerate() {
            if labels.iter().filter(|l| **l == labels[i]).count() > 1 && *k < parts(p).len() {
                *k += 1;
                grew = true;
            }
        }
        if !grew {
            break;
        }
    }
    let mut texts: Vec<(String, String, Option<&str>)> = Vec::with_capacity(files.len());
    for (p, k, owner) in &files {
        let text = std::fs::read_to_string(p).map_err(|e| format!("{}: {e}", p.display()))?;
        texts.push((label(p, *k), text, *owner));
    }
    let files: Vec<(&str, &str, Option<&str>)> = texts
        .iter()
        .map(|(n, t, o)| (n.as_str(), t.as_str(), *o))
        .collect();
    let mut kinds = compile_owned(&files, consts)?;
    kinds.debug.packs = full_paths
        .iter()
        .map(|(_, f, owner)| {
            let f = f.to_string_lossy();
            owner.map_or_else(|| f.clone().into_owned(), |o| format!("{o}={f}"))
        })
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

const SYMS: [&str; 25] = [
    "=>", "==", "!=", "<=", ">=", "+=", "-=", "{", "}", "(", ")", ",", ":", "=", "<", ">", "+",
    "-", "*", "/", "%", ";", ".", "[", "]",
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

    fn err_at(&self, line: u32, col: u32, msg: impl Into<String>) -> CompileError {
        CompileError {
            file: self.file.to_string(),
            line,
            col,
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
                        return Err(self.err_at(line, col, "number too large"));
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
                        ticks(minutes(v as u64))
                            .ok_or_else(|| self.err_at(line, col, "time too long"))?,
                    ),
                    "h" => Tok::Time(
                        ticks(hours(v as u64))
                            .ok_or_else(|| self.err_at(line, col, "time too long"))?,
                    ),
                    "d" => Tok::Time(
                        ticks(days(v as u64))
                            .ok_or_else(|| self.err_at(line, col, "time too long"))?,
                    ),
                    u => {
                        return Err(self.err_at(
                            line,
                            col,
                            format!("unknown unit `{u}` (min, h, d)"),
                        ));
                    }
                }
            } else if b.is_ascii_alphabetic() || b == b'_' {
                let mut s = String::new();
                while self.peek(0).is_ascii_alphanumeric() || self.peek(0) == b'_' {
                    s.push(char::from(self.bump()));
                }
                Tok::Name(s)
            } else if b == b'"' {
                self.bump();
                let start = self.at;
                loop {
                    match self.peek(0) {
                        b'"' => break,
                        _ if self.end() => {
                            return Err(self.err_at(line, col, "unterminated string"));
                        }
                        b'\n' => return Err(self.err_at(line, col, "unterminated string")),
                        _ => {
                            self.bump();
                        }
                    }
                }
                // Both ends sit on an ASCII `"`: a whole UTF-8 slice.
                let s = self.text[start..self.at].to_string();
                self.bump();
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
    /// A search's plan (`as plan`): `.via`, `.cost`, `.steps`.
    Plan,
}

impl Ty {
    /// Stack values / locals it occupies.
    fn width(self) -> u8 {
        match self {
            Ty::Plan => 3,
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
    solid: bool,
    cadence: Option<(Expr, Pos)>,
    sight: Option<(Expr, Pos)>,
    fuel: Option<(Expr, Pos)>,
    food: Option<(Expr, Pos)>,
    bite: Option<(Expr, Pos)>,
    /// `layer NAME`: a declared layer its rows live in.
    layer: Option<(String, Pos)>,
    /// `reach N`: how far `eat`, `hit` and `graze` reach.
    reach: Option<(Expr, Pos)>,
    /// `climb N on F`: the most a step may rise in field F.
    climb: Option<(Expr, String, Pos)>,
    /// `group max N [init M]`: a row stands for up to N heads.
    group: Option<(Expr, Option<Expr>, Pos)>,
    /// `size W H`: the cells it covers.
    size: Option<(Expr, Expr, Pos)>,
    tags: Vec<String>,
    needs: Vec<NeedAst>,
    mems: Vec<(String, Pos)>,
    genes: Vec<GeneAst>,
}

/// A gene as an instance holds it: by name (its slot is in `mems`).
#[derive(Debug, Clone, PartialEq, Eq)]
struct GeneInst {
    name: String,
    default: i32,
    lo: i32,
    hi: i32,
    mutate: u8,
}

/// `gene NAME = DEFAULT from LO to HI [mutate P]`.
#[derive(Debug, Clone)]
struct GeneAst {
    name: String,
    default: Expr,
    lo: Expr,
    hi: Expr,
    mutate: Option<Expr>,
    at: Pos,
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
    fields: Vec<FieldAst>,
    materials: Vec<MaterialAst>,
    /// `layer NAME { flies  on KIND }`: declared layers, with where, and the
    /// kind or tag each rests on as written (`LayerDef::on` is resolved
    /// once kinds are numbered).
    layers: Vec<(LayerDef, Pos, Option<(String, Pos)>)>,
    /// `budget { fuel N  rows N  private N }`: the arena's room for players.
    budgets: Vec<BudgetAst>,
}

/// `budget { fuel N  rows N  private N }`, each part optional: what an
/// arena leaves each player (`docs/RULES.md` §15). Numbers are constant
/// expressions.
#[derive(Debug, Clone)]
struct BudgetAst {
    at: Pos,
    fuel: Option<(Expr, Pos)>,
    rows: Option<(Expr, Pos)>,
    private: Option<(Expr, Pos)>,
}

/// `ground NAME { walkable  drink  glyph "c"  color "#rrggbb" }` or
/// `feature NAME { blocks  opaque  glyph "c"  color "#rrggbb" }` (`docs/WORLD.md`
/// §5): a new material, or the engine's soil, water or rock redeclared.
#[derive(Debug, Clone)]
struct MaterialAst {
    at: Pos,
    ground: bool,
    name: String,
    /// A ground: `walkable`. A feature: `blocks`.
    solid: bool,
    drink: bool,
    /// A feature: `opaque`, it stops a line of sight.
    opaque: bool,
    glyph: Option<u8>,
    color: Option<u32>,
}

/// `field NAME max M [conserved|fixed] { init V | init noise scale S; color "#rrggbb" }`:
/// a number in every cell (`docs/WORLD.md` §2). Numbers are constant
/// expressions, folded after the constants.
#[derive(Debug, Clone)]
struct FieldAst {
    at: Pos,
    name: String,
    max: Expr,
    policy: Policy,
    /// `init V`, or `init noise scale S` (`true`); none: 0.
    init: Option<(bool, Expr, Pos)>,
    color: Option<u32>,
    /// `outside V`: what its law reads where nothing is loaded.
    outside: Option<(Expr, Pos)>,
    /// `every N { body }`: its law.
    law: Option<(Expr, Vec<Stmt>, Pos)>,
    /// `relief`: drawn with its slopes shaded.
    relief: bool,
    /// `private`: a player's own field (`docs/WORLD.md` §12).
    private: bool,
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
    /// A search's body is its one [`Stmt::Search`].
    body: Vec<Stmt>,
    /// Has a `return expr` somewhere: a function. A search returns
    /// whether it found its goal.
    returns: bool,
}

impl SubAst {
    fn is_search(&self) -> bool {
        matches!(self.body.as_slice(), [Stmt::Search(_)])
    }
}

/// `search NAME(params) { state a, b = e, f  next { ... try ... }  goal
/// cond  guess e  limit N }` (`docs/RULES.md` §12, Searches).
#[derive(Debug, Clone)]
struct SearchAst {
    at: Pos,
    state: Vec<(String, Pos)>,
    start: Vec<Expr>,
    next: Vec<Stmt>,
    goal: Cond,
    guess: Option<Expr>,
    limit: Option<(Expr, Pos)>,
}

#[derive(Debug, Clone)]
struct Rule {
    /// Where its `when` is, and the line of its `=>`.
    at: Pos,
    arrow_line: u32,
    /// Where the token after its last one is, as (line, column).
    end: (u32, u32),
    cond: Cond,
    body: Vec<Stmt>,
    /// Each term of the condition's top-level `and`: where it starts and
    /// where the `and` (or the `=>`) after it is, as (line, column).
    terms: Vec<((u32, u32), (u32, u32))>,
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
    /// `best f [on pred] within r as v`: the cell with the most of field
    /// `f` (among those matching `pred`).
    Best {
        field: String,
        pred: Option<Pred>,
        r: Expr,
        bind: String,
        at: Pos,
    },
    /// `NAME(args) as plan [into route]`: a search that found its goal,
    /// its plan bound.
    Search {
        name: String,
        args: Vec<Arg>,
        bind: String,
        into: Option<String>,
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
    /// A search's whole body (see [`SearchAst`]).
    Search(Box<SearchAst>),
    /// `try a, b [cost c] [via v]`: in a search's `next`, one state it may
    /// go to.
    Try {
        values: Vec<Expr>,
        cost: Option<Expr>,
        via: Option<Expr>,
        at: Pos,
    },
    /// `route[i] = v`, `+=`, `-=`: an element of a memory array.
    SetAt {
        name: String,
        at: Pos,
        index: Expr,
        op: Option<OpCode>,
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
    /// `dig t`: clear the feature at `t`.
    Dig(Target, Pos),
    /// `split n at t`: a group's `n` heads start a new group at `t`.
    Split {
        amount: Expr,
        target: Target,
        at: Pos,
    },
    /// `join t`: a group gives its heads to the group of its kind at `t`.
    Join(Target, Pos),
    /// `grow n`: an effect, a group's heads change by `n`.
    Grow(Expr, Pos),
    /// `build M at t`: lay ground or feature `M` on the free cell `t`.
    Build {
        material: String,
        target: Target,
        at: Pos,
    },
    Drink(Target, Pos),
    Eat(Target, Pos),
    Hit(Target, Pos),
    Graze(Target, Pos),
    /// `look = v`: an effect, not an action.
    Look(Expr),
    /// `signal = v`: an effect, not an action.
    Signal(Expr),
    /// `add f v`: an effect, adds `v` to field `f` on the actor's cell.
    Add(String, Expr, Pos),
    /// `harvest f n` / `drop f n`: up to `n` of field `f` on the actor's
    /// cell into its need `f`, or back.
    Harvest {
        drop: bool,
        field: String,
        amount: Expr,
        at: Pos,
    },
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
    /// `(F op v and ... [and pred])`: cells where each field compares true
    /// (the value computed once, before the search), and the predicate holds.
    Fields(Vec<(String, OpCode, Expr, Pos)>, Option<Box<Pred>>, Pos),
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
    /// `fits(t)`: the thinker's footprint fits with its anchor at `t`.
    FitsAt(Target),
    IsAt(Target, Pred),
    /// `look_of(t)`, `signal_of(t)`: the public bytes of whoever is there.
    LookOf(Target),
    SignalOf(Target, Pos),
    /// `heads_of(t)`: the heads of whoever is at `t` (1 for one who is not
    /// a group, 0 for nobody).
    HeadsOf(Target),
    /// `min`, `max`, `abs`, `sign`, `clamp`, `pack`, `hi`, `lo`: the opcode
    /// and its arguments.
    Fn(OpCode, Vec<Expr>),
    Call {
        name: String,
        args: Vec<Arg>,
        at: Pos,
    },
    /// `sum|min|max F along t`: field F over the line to `t` (op 0, 1, 2).
    Along {
        op: u8,
        field: String,
        target: Box<Target>,
        at: Pos,
    },
    /// `sees(t) over F [eye N]`: line of sight over a heightfield.
    Sees {
        target: Box<Target>,
        field: String,
        eye: Option<Box<Expr>>,
        at: Pos,
    },
    /// In a field's law only: `sum|avg|min|max F within r` over the 3 x 3
    /// square, or `count (cond) within r` (the cells where it holds).
    Agg {
        op: AggOp,
        of: Box<AggOf>,
        r: Box<Expr>,
        at: Pos,
    },
    /// `route[i]`: an element of a memory array (0 past its ends).
    Index(String, Box<Expr>, Pos),
    /// In a field's law only: `flow(w, level, rate)`, what flows in from
    /// the four neighbours.
    Flow {
        of: Box<Expr>,
        level: Box<Expr>,
        rate: Box<Expr>,
        at: Pos,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AggOp {
    Sum,
    Avg,
    Min,
    Max,
    Count,
}

#[derive(Debug, Clone)]
enum AggOf {
    Field(String, Pos),
    Cond(Box<Cond>),
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
    /// While a rule's `when` is parsed, the depth of its top level; there,
    /// the first token of each term of the top-level `and`.
    top: Option<u32>,
    conjuncts: Vec<usize>,
    /// Parsing a field's law: aggregates over the 3 x 3 square parse.
    law: bool,
}

/// How deep one tree may be: a nesting and an operator in a chain are one
/// level each (RULES §18). The parser, the code generator, lint and drop
/// all recurse on the tree, so this is what keeps them on the stack.
pub const MAX_DEPTH: u32 = 128;

// The other limits of a rule set (RULES §18; `tests/docs.rs` holds the doc
// to these).
/// The widest `sight`: one chunk, so a search from any cell stays inside
/// the 3x3 chunk halo.
pub const MAX_SIGHT: u8 = 64;
/// The most `fuel` a kind may declare, in ops per think.
pub const MAX_FUEL: u32 = 32768;
/// `state` blocks per kind.
pub const MAX_STATES: usize = 64;
/// Fields in a rule set (`field`): a byte indexes them in the bytecode.
pub const MAX_FIELDS: usize = 64;
/// Tags per rule set: a kind's tags are one `u64` bitset.
pub const MAX_TAGS: usize = u64::BITS as usize;
/// Cells a big actor covers each way (`size W H`): a part keeps its offset
/// from the anchor in 4 bits (`ActorId::part`).
pub const MAX_SIZE: i32 = 16;
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
        "heads" => Sense::Heads,
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
    "gene",
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
    "dig",
    "build",
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
    "solid",
    "graze",
    "state",
    "next",
    "const",
    "for",
    "each",
    "look_of",
    "signal_of",
    "heads_of",
    "fits",
    "grow",
    "split",
    "join",
    "pack",
    "hi",
    "lo",
    "take",
    "give",
    "with",
    "field",
    "add",
    "harvest",
    "drop",
    "best",
    "sees",
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
    "flow",
    "search",
    "try",
    "dist",
    "is",
    "free",
];
/// Words that start a declaration in a kind or trait body.
const DECL_WORDS: [&str; 19] = [
    "glyph", "tags", "cadence", "sight", "fuel", "food", "bite", "cover", "solid", "color",
    "place", "need", "mem", "reach", "climb", "layer", "group", "size", "gene",
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
        // `namespace NAME` first: compile_owned has applied it to the
        // tokens (and taken it out); parsed alone, it is skipped.
        if self.is_kw("namespace")
            && matches!(self.tokens.get(1).map(|t| &t.tok), Some(Tok::Name(_)))
        {
            self.bump();
            self.bump();
        }
        while *self.peek() != Tok::Eof {
            if self.is_kw("kind") || self.is_kw("trait") {
                items.items.push(self.item()?);
            } else if self.is_kw("sub") {
                items.subs.push(self.sub()?);
            } else if self.is_kw("search") {
                items.subs.push(self.search()?);
            } else if self.eat_kw("const") {
                let (name, at) = self.ident("constant name")?;
                self.expect_sym("=")?;
                let value = self.expr()?;
                self.eat_sym(";");
                items.consts.push(ConstAst { at, name, value });
            } else if self.eat_kw("field") {
                items.fields.push(self.field()?);
            } else if self.is_kw("ground") || self.is_kw("feature") {
                items.materials.push(self.material()?);
            } else if self.eat_kw("layer") {
                let (name, at) = self.ident("layer name")?;
                let (mut flies, mut on) = (false, None);
                if self.eat_sym("{") {
                    while !self.eat_sym("}") {
                        if self.eat_kw("flies") {
                            flies = true;
                        } else if self.is_kw("on") {
                            if on.is_some() {
                                return Err(self.err("`on` given twice in a layer"));
                            }
                            self.bump();
                            on = Some(self.ident("kind or tag")?);
                        } else {
                            return Err(self.err(format!(
                                "expected `flies`, `on` or `}}` in a layer, found {}",
                                self.describe()
                            )));
                        }
                    }
                }
                let def = LayerDef {
                    name,
                    flies,
                    on: None,
                };
                items.layers.push((def, at, on));
            } else if self.is_kw("budget") {
                let at = self.pos();
                self.bump();
                items.budgets.push(self.budget(at)?);
            } else {
                return Err(self.err(format!(
                    "expected `kind`, `trait`, `sub`, `const`, `field`, `ground`, `feature`, `layer` or `budget`, found {}",
                    self.describe()
                )));
            }
        }
        Ok(())
    }

    /// The block of a `budget`, after the word.
    fn budget(&mut self, at: Pos) -> Result<BudgetAst> {
        let mut b = BudgetAst {
            at,
            fuel: None,
            rows: None,
            private: None,
        };
        self.expect_sym("{")?;
        while !self.eat_sym("}") {
            let p = self.pos();
            let slot = if self.eat_kw("fuel") {
                &mut b.fuel
            } else if self.eat_kw("rows") {
                &mut b.rows
            } else if self.eat_kw("private") {
                &mut b.private
            } else {
                return Err(self.err(format!(
                    "expected `fuel`, `rows`, `private` or `}}` in a budget, found {}",
                    self.describe()
                )));
            };
            if slot.is_some() {
                return Err(self.err_at(&p, "given twice in the budget"));
            }
            let e = self.additive()?;
            *slot = Some((e, p));
        }
        Ok(b)
    }

    /// `ground NAME { ... }` or `feature NAME { ... }`.
    fn material(&mut self) -> Result<MaterialAst> {
        let ground = self.is_kw("ground");
        self.bump();
        let (name, at) = self.ident(if ground {
            "ground name"
        } else {
            "feature name"
        })?;
        let mut m = MaterialAst {
            at,
            ground,
            name,
            solid: false,
            drink: false,
            opaque: false,
            glyph: None,
            color: None,
        };
        self.expect_sym("{")?;
        while !self.eat_sym("}") {
            let p = self.pos();
            let word = if ground { "walkable" } else { "blocks" };
            if self.eat_kw(word) {
                m.solid = true;
            } else if ground && self.eat_kw("drink") {
                m.drink = true;
            } else if !ground && self.eat_kw("opaque") {
                m.opaque = true;
            } else if (self.is_kw("glyph") && m.glyph.is_some())
                || (self.is_kw("color") && m.color.is_some())
            {
                let what = if ground { "ground" } else { "feature" };
                return Err(self.err(format!("{} given twice in a {what}", self.describe())));
            } else if self.eat_kw("glyph") {
                m.glyph = Some(match self.bump() {
                    Tok::Str(s) if s.len() == 1 && s.as_bytes()[0].is_ascii_graphic() => {
                        s.as_bytes()[0]
                    }
                    _ => return Err(self.err_at(&p, "glyph takes one printable character")),
                });
            } else if self.eat_kw("color") {
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
                m.color = Some(c.ok_or_else(|| self.err_at(&p, "color takes \"#rrggbb\""))?);
            } else {
                let more = if ground {
                    "`walkable`, `drink`"
                } else {
                    "`blocks`, `opaque`"
                };
                return Err(self.err(format!(
                    "expected {more}, `glyph`, `color` or `}}` in a {}, found {}",
                    if ground { "ground" } else { "feature" },
                    self.describe()
                )));
            }
        }
        Ok(m)
    }

    /// After `field`: its name, `max`, a policy word and an optional block
    /// of `init` and `color`.
    fn field(&mut self) -> Result<FieldAst> {
        let (name, at) = self.ident("field name")?;
        self.expect_kw("max")?;
        let max = self.additive()?;
        let policy = if self.eat_kw("conserved") {
            Policy::Conserved
        } else if self.eat_kw("fixed") {
            Policy::Fixed
        } else {
            Policy::Free
        };
        let private = self.eat_kw("private");
        let (mut init, mut color, mut outside, mut law) = (None, None, None, None);
        let mut relief = false;
        if self.eat_sym("{") {
            while !self.eat_sym("}") {
                let p = self.pos();
                if self.eat_kw("relief") {
                    if relief {
                        return Err(self.err_at(&p, "`relief` declared twice"));
                    }
                    relief = true;
                    continue;
                }
                if self.eat_kw("outside") {
                    if outside.is_some() {
                        return Err(self.err_at(&p, "`outside` declared twice"));
                    }
                    outside = Some((self.additive()?, p));
                    continue;
                }
                if self.eat_kw("every") {
                    if law.is_some() {
                        return Err(self.err_at(&p, "a field has one law: `every` given twice"));
                    }
                    let every = self.additive()?;
                    self.law = true;
                    let body = self.block();
                    self.law = false;
                    law = Some((every, body?, p));
                    continue;
                }
                if self.eat_kw("init") {
                    if init.is_some() {
                        return Err(self.err_at(&p, "`init` declared twice"));
                    }
                    let noise = self.eat_kw("noise");
                    if noise {
                        self.expect_kw("scale")?;
                    }
                    init = Some((noise, self.additive()?, p));
                } else if self.eat_kw("color") {
                    if color.is_some() {
                        return Err(self.err_at(&p, "`color` declared twice"));
                    }
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
                    color = Some(c.ok_or_else(|| self.err_at(&p, "color takes \"#rrggbb\""))?);
                } else {
                    return Err(self.err(format!(
                        "expected `init`, `color`, `outside`, `every`, `relief` or `}}` in a field, found {}",
                        self.describe()
                    )));
                }
            }
        }
        Ok(FieldAst {
            at,
            name,
            max,
            policy,
            init,
            color,
            outside,
            law,
            relief,
            private,
        })
    }

    /// Does name `n`, just taken, start an aggregate here? `sum` and `avg`
    /// before a name, `min` and `max` before a name (not a call), `count`
    /// before `(`.
    fn agg_start(&self, n: &str) -> Option<AggOp> {
        let name_next = matches!(self.peek(), Tok::Name(_));
        match n {
            "sum" if name_next => Some(AggOp::Sum),
            "avg" if name_next => Some(AggOp::Avg),
            "min" if name_next => Some(AggOp::Min),
            "max" if name_next => Some(AggOp::Max),
            "count" if self.is_sym("(") => Some(AggOp::Count),
            _ => None,
        }
    }

    /// The rest of an aggregate: its field or `(cond)`, `within`, a radius.
    fn agg(&mut self, op: AggOp, at: Pos) -> Result<Expr> {
        let of = if op == AggOp::Count {
            self.expect_sym("(")?;
            let c = self.nested(Self::cond)?;
            self.expect_sym(")")?;
            AggOf::Cond(Box::new(c))
        } else {
            let (f, p) = self.ident("field name")?;
            AggOf::Field(f, p)
        };
        self.expect_kw("within")?;
        let r = self.unary()?;
        Ok(Expr::Agg {
            op,
            of: Box::new(of),
            r: Box::new(r),
            at,
        })
    }

    fn sub(&mut self) -> Result<SubAst> {
        self.expect_kw("sub")?;
        let (name, at) = self.ident("sub name")?;
        let params = self.params()?;
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

    /// `search NAME[(params)] { state ... next { ... } goal ... [guess e]
    /// [limit N] }`, each part once, `state`, `next` and `goal` required.
    fn search(&mut self) -> Result<SubAst> {
        self.expect_kw("search")?;
        let (name, at) = self.ident("search name")?;
        let params = if self.is_sym("(") {
            self.params()?
        } else {
            Vec::new()
        };
        self.expect_sym("{")?;
        let (mut state, mut start, mut next, mut goal, mut guess, mut limit) =
            (Vec::new(), Vec::new(), None, None, None, None);
        while !self.eat_sym("}") {
            let p = self.pos();
            let twice = |done: bool, what: &str| {
                done.then(|| format!("`{what}` given twice in search `{name}`"))
            };
            if self.eat_kw("state") {
                if let Some(e) = twice(!state.is_empty(), "state") {
                    return Err(self.err_at(&p, e));
                }
                loop {
                    let (n, nat) = self.ident("state name")?;
                    if state.iter().any(|(m, _)| *m == n) {
                        return Err(self.err_at(&nat, format!("`{n}` named twice in the state")));
                    }
                    state.push((n, nat));
                    if !self.eat_sym(",") {
                        break;
                    }
                }
                self.expect_sym("=")?;
                loop {
                    start.push(self.expr()?);
                    if !self.eat_sym(",") {
                        break;
                    }
                }
                if start.len() != state.len() {
                    return Err(self.err_at(
                        &p,
                        format!(
                            "{} state names, {} starting values",
                            state.len(),
                            start.len()
                        ),
                    ));
                }
                if state.len() > SEARCH_STATE {
                    return Err(self.err_at(
                        &p,
                        format!("a search's state holds at most {SEARCH_STATE} numbers"),
                    ));
                }
            } else if self.eat_kw("next") {
                if let Some(e) = twice(next.is_some(), "next") {
                    return Err(self.err_at(&p, e));
                }
                next = Some(self.block()?);
            } else if self.eat_kw("goal") {
                if let Some(e) = twice(goal.is_some(), "goal") {
                    return Err(self.err_at(&p, e));
                }
                goal = Some(self.cond()?);
            } else if self.eat_kw("guess") {
                if let Some(e) = twice(guess.is_some(), "guess") {
                    return Err(self.err_at(&p, e));
                }
                guess = Some(self.expr()?);
            } else if self.eat_kw("limit") {
                if let Some(e) = twice(limit.is_some(), "limit") {
                    return Err(self.err_at(&p, e));
                }
                limit = Some((self.additive()?, p));
            } else {
                return Err(self.err(format!(
                    "expected `state`, `next`, `goal`, `guess`, `limit` or `}}` in search `{name}`, found {}",
                    self.describe()
                )));
            }
        }
        let (Some(next), Some(goal)) = (next, goal) else {
            return Err(self.err_at(
                &at,
                format!("search `{name}` needs a `state`, a `next` and a `goal`"),
            ));
        };
        if state.is_empty() {
            return Err(self.err_at(
                &at,
                format!("search `{name}` needs a `state`, a `next` and a `goal`"),
            ));
        }
        let s = SearchAst {
            at: at.clone(),
            state,
            start,
            next,
            goal,
            guess,
            limit,
        };
        Ok(SubAst {
            at,
            name,
            params,
            body: vec![Stmt::Search(Box::new(s))],
            returns: true,
        })
    }

    /// `(name[: type], ...)`: a sub's or a search's parameters.
    fn params(&mut self) -> Result<Vec<(String, Ty)>> {
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
        Ok(params)
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
        while self.is_kw("sub") || self.is_kw("search") {
            let sub = if self.is_kw("sub") {
                self.sub()?
            } else {
                self.search()?
            };
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
            } else if self.is_kw("sub") || self.is_kw("search") {
                "member subs and searches come before the rules".to_string()
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
                ("solid", d.solid),
                ("cadence", d.cadence.is_some()),
                ("sight", d.sight.is_some()),
                ("fuel", d.fuel.is_some()),
                ("food", d.food.is_some()),
                ("bite", d.bite.is_some()),
                ("reach", d.reach.is_some()),
                ("layer", d.layer.is_some()),
                ("climb", d.climb.is_some()),
                ("size", d.size.is_some()),
                ("group", d.group.is_some()),
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
                        || ["when", "inherit", "state", "sub", "search"].contains(&n.as_str())
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
            } else if self.eat_kw("reach") {
                d.reach = Some((self.additive()?, p));
            } else if self.eat_kw("layer") {
                let (name, _) = self.ident("layer name")?;
                d.layer = Some((name, p));
            } else if self.eat_kw("climb") {
                let n = self.additive()?;
                self.expect_kw("on")?;
                let (f, _) = self.ident("field name")?;
                d.climb = Some((n, f, p));
            } else if self.eat_kw("size") {
                let w = self.additive()?;
                let h = self.additive()?;
                d.size = Some((w, h, p));
            } else if self.eat_kw("group") {
                self.expect_kw("max")?;
                let max = self.additive()?;
                let init = if self.eat_kw("init") {
                    Some(self.additive()?)
                } else {
                    None
                };
                d.group = Some((max, init, p));
            } else if self.eat_kw("cover") {
                d.cover = true;
            } else if self.eat_kw("solid") {
                d.solid = true;
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
                    let at = self.pos();
                    match self.int("0 (points) or 1 (per tick)")? {
                        0 => decays = false,
                        1 => decays = true,
                        _ => return Err(self.err_at(&at, "decay is 0 (points) or 1 (per tick)")),
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
            } else if self.eat_kw("gene") {
                let (name, ..) = self.ident("gene name")?;
                if d.genes.iter().any(|g| g.name == name)
                    || d.needs.iter().any(|n| n.name == name)
                    || d.mems
                        .iter()
                        .any(|(m, _)| *m == name || array_of(m) == Some(&name))
                {
                    return Err(self.err_at(&p, format!("`{name}` declared twice")));
                }
                self.expect_sym("=")?;
                let default = self.additive()?;
                self.expect_kw("from")?;
                let lo = self.additive()?;
                self.expect_kw("to")?;
                let hi = self.additive()?;
                let mutate = if self.eat_kw("mutate") {
                    Some(self.additive()?)
                } else {
                    None
                };
                d.genes.push(GeneAst {
                    name,
                    default,
                    lo,
                    hi,
                    mutate,
                    at: p,
                });
            } else if self.eat_kw("mem") {
                loop {
                    let (name, at) = self.ident("memory slot name")?;
                    let taken = |n: &str| {
                        d.mems.iter().any(|(m, _)| m == n || array_of(m) == Some(n))
                            || d.needs.iter().any(|x| x.name == n)
                            || d.genes.iter().any(|g| g.name == n)
                    };
                    if taken(&name) {
                        return Err(self.err_at(&at, format!("`{name}` declared twice")));
                    }
                    // `mem route[8]`: eight slots, `route[0]` to `route[7]`.
                    if self.eat_sym("[") {
                        let len = match self.bump() {
                            Tok::Int(n)
                                if usize::try_from(n)
                                    .is_ok_and(|n| (1..=MEM_SLOTS).contains(&n)) =>
                            {
                                n
                            }
                            _ => {
                                return Err(self.err_at(
                                    &at,
                                    format!("`{name}[N]`: N is a number, 1 to {MEM_SLOTS}"),
                                ));
                            }
                        };
                        self.expect_sym("]")?;
                        for k in 0..len {
                            d.mems.push((format!("{name}[{k}]"), at.clone()));
                        }
                    } else {
                        d.mems.push((name, at));
                    }
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
            let first = self.at;
            self.top = Some(self.depth);
            let cond = self.cond();
            self.top = None;
            let cond = cond?;
            let arrow = self.at;
            let arrow_line = self.pos().line;
            self.expect_sym("=>")?;
            // Each term ends at the `and` before the next, the last at `=>`.
            let starts = if matches!(cond, Cond::Or(..)) {
                vec![first]
            } else {
                std::mem::take(&mut self.conjuncts)
            };
            let line_col = |i: usize| (self.tokens[i].line, self.tokens[i].col);
            let terms = starts
                .iter()
                .zip(starts.iter().skip(1).map(|&s| s - 1).chain([arrow]))
                .map(|(&a, b)| (line_col(a), line_col(b)))
                .collect();
            // One statement may end in `;`, as in a block; a block may not.
            let single = !self.is_sym("{");
            let body = self.body()?;
            let end = self.pos();
            if single {
                self.eat_sym(";");
            }
            rules.push(RuleItem::When(Box::new(Rule {
                at,
                arrow_line,
                end: (end.line, end.col),
                cond,
                body,
                terms,
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
        let open = self.pos();
        self.expect_sym("{")?;
        let mut stmts = Vec::new();
        while !self.is_sym("}") {
            if *self.peek() == Tok::Eof {
                return Err(self.err_at(&open, "unclosed block"));
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
            let value = self.value()?;
            return Ok(Stmt::Let { name, at, value });
        }
        if self.eat_kw("return") {
            // The value starts on the `return` line, or there is none.
            let value = if self.is_sym("}") || self.is_sym(";") || self.pos().line != at.line {
                None
            } else {
                Some(self.value()?)
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
        if self.eat_kw("split") {
            let amount = self.expr()?;
            self.expect_kw("at")?;
            let target = self.target()?;
            return Ok(Stmt::Split { amount, target, at });
        }
        if self.eat_kw("join") {
            return Ok(Stmt::Join(self.target()?, at));
        }
        if self.eat_kw("grow") {
            return Ok(Stmt::Grow(self.expr()?, at));
        }
        if self.eat_kw("dig") {
            return Ok(Stmt::Dig(self.target()?, at));
        }
        if self.eat_kw("build") {
            let (material, _) = self.ident("ground or feature name")?;
            self.expect_kw("at")?;
            let target = self.target()?;
            return Ok(Stmt::Build {
                material,
                target,
                at,
            });
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
            return Ok(Stmt::Look(self.value()?));
        }
        if self.is_kw("signal") && matches!(self.peek2(), Tok::Sym("=")) {
            self.bump();
            self.bump();
            return Ok(Stmt::Signal(self.value()?));
        }
        if self.eat_kw("next") {
            let (name, at) = self.ident("state name")?;
            return Ok(Stmt::Next(name, at));
        }
        if self.eat_kw("add") {
            let (field, at) = self.ident("field name")?;
            return Ok(Stmt::Add(field, self.value()?, at));
        }
        if self.is_kw("harvest") || self.is_kw("drop") {
            let drop = self.is_kw("drop");
            self.bump();
            let (field, _) = self.ident("field name")?;
            let amount = self.expr()?;
            return Ok(Stmt::Harvest {
                drop,
                field,
                amount,
                at,
            });
        }
        if self.eat_kw("try") {
            let mut values = vec![self.expr()?];
            while self.eat_sym(",") {
                values.push(self.expr()?);
            }
            let cost = if self.eat_kw("cost") {
                Some(self.expr()?)
            } else {
                None
            };
            let via = if self.eat_kw("via") {
                Some(self.expr()?)
            } else {
                None
            };
            return Ok(Stmt::Try {
                values,
                cost,
                via,
                at,
            });
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
        if self.eat_sym("[") {
            let index = self.expr()?;
            self.expect_sym("]")?;
            let op = if self.eat_sym("=") {
                None
            } else if self.eat_sym("+=") {
                Some(OpCode::Add)
            } else if self.eat_sym("-=") {
                Some(OpCode::Sub)
            } else {
                return Err(self.err(format!(
                    "expected `=`, `+=` or `-=` after `{name}[...]`, found {}",
                    self.describe()
                )));
            };
            let value = self.value()?;
            return Ok(Stmt::SetAt {
                name,
                at,
                index,
                op,
                value,
            });
        }
        if self.eat_sym("=") {
            let value = self.value()?;
            return Ok(Stmt::Set { name, at, value });
        }
        for (sym, op) in [("+=", OpCode::Add), ("-=", OpCode::Sub)] {
            if self.eat_sym(sym) {
                let value = self.value()?;
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

    /// The value a statement stores or returns: an expression, where an
    /// `and` or `or` after it is a condition written for a number.
    fn value(&mut self) -> Result<Expr> {
        let e = self.expr()?;
        if self.is_kw("and") || self.is_kw("or") {
            return Err(self.err(format!(
                "{} joins conditions only (in `when`, `if` and `while`), not a value: \
                 `if a and b {{ x = 1 }}`",
                self.describe()
            )));
        }
        Ok(e)
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
        let top = self.top == Some(self.depth);
        let mut starts = vec![self.at];
        let mut c = self.not_cond()?;
        while self.eat_kw("and") {
            starts.push(self.at);
            let left = std::mem::replace(&mut self.deepest, self.depth);
            let r = self.not_cond()?;
            self.chained(left)?;
            c = Cond::And(Box::new(c), Box::new(r));
        }
        if top {
            self.conjuncts = starts;
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
        if self.eat_kw("best") {
            let (field, _) = self.ident("field name")?;
            let pred = if self.eat_kw("on") {
                Some(self.pred()?)
            } else {
                None
            };
            self.expect_kw("within")?;
            let r = self.additive()?; // a radius, never a comparison
            self.expect_kw("as")?;
            let (bind, ..) = self.ident("binding name")?;
            return Ok(Cond::Best {
                field,
                pred,
                r,
                bind,
                at,
            });
        }
        if self.is_sym("(") {
            // Either a parenthesised condition or a parenthesised expression
            // starting a comparison. A condition holds every expression, so
            // parse a condition; an expression in it followed by an operator
            // goes on from there as an expression. Never a second parse: in
            // a law an expression holds conditions (`count (c) within r`),
            // and parsing each level twice is exponential in the nesting.
            self.bump();
            let c = self.nested(Self::cond)?;
            self.expect_sym(")")?;
            if !self.starts_binop() {
                return Ok(c);
            }
            let Cond::Expr(e) = c else {
                return Err(self.err(format!(
                    "{} after a condition: `and`, `or` and `not` make conditions, not numbers",
                    self.describe()
                )));
            };
            let reached = self.deepest;
            return Ok(Cond::Expr(self.expr_with(Some((e, reached)))?));
        }
        let e = self.expr()?;
        // `name(args) as plan [into route]`: a search's plan.
        if self.is_kw("as")
            && let Expr::Call { name, args, at } = e
        {
            self.bump();
            let (bind, ..) = self.ident("binding name")?;
            let into = if self.eat_kw("into") {
                Some(self.ident("memory array name")?.0)
            } else {
                None
            };
            return Ok(Cond::Search {
                name,
                args,
                bind,
                into,
                at,
            });
        }
        Ok(Cond::Expr(e))
    }

    fn starts_binop(&self) -> bool {
        matches!(self.peek(), Tok::Sym(s) if ["+", "-", "*", "/", "%", "<", "<=", "==", "!=", ">=", ">"].contains(s))
    }

    fn pred(&mut self) -> Result<Pred> {
        let at = self.pos();
        if self.eat_sym("(") {
            // Nested like any parenthesis: `((((...` is an error, not a stack
            // overflow (a fuzz find).
            return self.nested(|p| p.field_pred(at));
        }
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

    /// After `(` where a predicate goes: field comparisons joined by `and`,
    /// at most one plain predicate among them, then `)`.
    fn field_pred(&mut self, at: Pos) -> Result<Pred> {
        let (mut terms, mut base) = (Vec::new(), None);
        loop {
            let p = self.pos();
            let field_term = matches!(self.peek(), Tok::Name(n) if !is_reserved(n))
                && matches!(
                    self.peek2(),
                    Tok::Sym("<" | "<=" | "==" | "!=" | ">=" | ">")
                );
            if field_term {
                let (f, _) = self.ident("field name")?;
                let op = self.cmp_op().expect("checked above");
                self.bump();
                terms.push((f, op, self.additive()?, p));
            } else {
                if base.is_some() {
                    return Err(self.err_at(&p, "a field condition holds one predicate at most"));
                }
                base = Some(Box::new(self.pred()?));
            }
            if !self.eat_kw("and") {
                break;
            }
        }
        self.expect_sym(")")?;
        if terms.is_empty() {
            return Err(self.err_at(
                &at,
                "a predicate in parentheses compares a field: `(sugar < 20)`",
            ));
        }
        if terms.len() > 3 {
            return Err(self.err_at(&at, "a field condition compares three fields at most"));
        }
        if matches!(base.as_deref(), Some(Pred::Fields(..))) {
            return Err(self.err_at(
                &at,
                "a field condition inside another: join them with `and`",
            ));
        }
        Ok(Pred::Fields(terms, base, at))
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
        self.expr_with(None)
    }

    /// An expression whose first operand, if given, is parsed already (with
    /// the depth it reached): a parenthesised one the caller read.
    fn expr_with(&mut self, first: Option<(Expr, u32)>) -> Result<Expr> {
        let outer = std::mem::replace(&mut self.deepest, self.depth);
        let mut e = self.additive_with(first)?;
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
        self.additive_with(None)
    }

    fn additive_with(&mut self, first: Option<(Expr, u32)>) -> Result<Expr> {
        let outer = std::mem::replace(&mut self.deepest, self.depth);
        let mut e = self.term_with(first)?;
        loop {
            let op = match self.peek() {
                Tok::Sym("+") => OpCode::Add,
                Tok::Sym("-") => OpCode::Sub,
                _ => break,
            };
            self.bump();
            let left = std::mem::replace(&mut self.deepest, self.depth);
            let r = self.term_with(None)?;
            self.chained(left)?;
            e = Expr::Bin(op, Box::new(e), Box::new(r));
        }
        self.deepest = self.deepest.max(outer);
        Ok(e)
    }

    fn term_with(&mut self, first: Option<(Expr, u32)>) -> Result<Expr> {
        let outer = std::mem::replace(&mut self.deepest, self.depth);
        let mut e = match first {
            Some((e, reached)) => {
                self.deepest = self.deepest.max(reached);
                e
            }
            None => self.unary()?,
        };
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
                if self.law
                    && let Some(op) = self.agg_start(&n)
                {
                    return self.agg(op, at);
                }
                if !self.law
                    && matches!(n.as_str(), "sum" | "min" | "max")
                    && matches!(self.peek(), Tok::Name(_))
                    && matches!(self.peek2(), Tok::Name(w) if w == "along")
                {
                    let (field, _) = self.ident("field name")?;
                    self.bump();
                    let target = Box::new(self.target()?);
                    let op = match n.as_str() {
                        "sum" => 0,
                        "min" => 1,
                        _ => 2,
                    };
                    return Ok(Expr::Along {
                        op,
                        field,
                        target,
                        at,
                    });
                }
                if n == "sees" {
                    self.expect_sym("(")?;
                    let target = Box::new(self.target()?);
                    self.expect_sym(")")?;
                    self.expect_kw("over")?;
                    let (field, _) = self.ident("field name")?;
                    let eye = if self.eat_kw("eye") {
                        Some(Box::new(self.unary()?))
                    } else {
                        None
                    };
                    return Ok(Expr::Sees {
                        target,
                        field,
                        eye,
                        at,
                    });
                }
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
                    "flow" => {
                        self.expect_sym("(")?;
                        let of = self.expr()?;
                        self.expect_sym(",")?;
                        let level = self.expr()?;
                        self.expect_sym(",")?;
                        let rate = self.expr()?;
                        self.expect_sym(")")?;
                        Ok(Expr::Flow {
                            of: Box::new(of),
                            level: Box::new(level),
                            rate: Box::new(rate),
                            at,
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
                    "fits" => {
                        self.expect_sym("(")?;
                        let t = self.target()?;
                        self.expect_sym(")")?;
                        Ok(Expr::FitsAt(t))
                    }
                    "is" => {
                        self.expect_sym("(")?;
                        let t = self.target()?;
                        self.expect_sym(",")?;
                        let p = self.pred()?;
                        self.expect_sym(")")?;
                        Ok(Expr::IsAt(t, p))
                    }
                    "look_of" | "signal_of" | "heads_of" => {
                        self.expect_sym("(")?;
                        let t = self.target()?;
                        self.expect_sym(")")?;
                        Ok(match n.as_str() {
                            "look_of" => Expr::LookOf(t),
                            "heads_of" => Expr::HeadsOf(t),
                            _ => Expr::SignalOf(t, at),
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
                        if self.eat_sym("[") {
                            let i = self.nested(Self::expr)?;
                            self.expect_sym("]")?;
                            return Ok(Expr::Index(n, Box::new(i), at));
                        }
                        if self.eat_sym(".") {
                            let field = match self.bump() {
                                Tok::Name(f) if f == "dx" => 0,
                                Tok::Name(f) if f == "dy" => 1,
                                // A plan's, from 10 (Ty::Plan's slots).
                                Tok::Name(f) if f == "via" => 10,
                                Tok::Name(f) if f == "cost" => 11,
                                Tok::Name(f) if f == "steps" => 12,
                                _ => {
                                    return Err(
                                        self.err_at(&at, format!("`{n}.` must be `.dx`, `.dy`, `.via`, `.cost` or `.steps`"))
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
        // Whether it found its goal.
        Stmt::Search(_) => true,
        Stmt::Try { .. }
        | Stmt::Set { .. }
        | Stmt::SetAt { .. }
        | Stmt::Assign { .. }
        | Stmt::Let { .. }
        | Stmt::Call { .. }
        | Stmt::Idle(_)
        | Stmt::Die(_)
        | Stmt::Become { .. }
        | Stmt::Spawn { .. }
        | Stmt::Transfer { .. }
        | Stmt::Split { .. }
        | Stmt::Join(..)
        | Stmt::Harvest { .. }
        | Stmt::Move(..)
        | Stmt::Dig(..)
        | Stmt::Build { .. }
        | Stmt::Drink(..)
        | Stmt::Eat(..)
        | Stmt::Hit(..)
        | Stmt::Graze(..)
        | Stmt::Look(_)
        | Stmt::Signal(_)
        | Stmt::Grow(..)
        | Stmt::Add(..)
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
            // A search never acts.
            Stmt::Search(_) | Stmt::Try { .. } => {}
            Stmt::Spawn { .. }
            | Stmt::Set { .. }
            | Stmt::SetAt { .. }
            | Stmt::Assign { .. }
            | Stmt::Let { .. }
            | Stmt::Call { .. }
            | Stmt::Return { .. }
            | Stmt::Idle(_)
            | Stmt::Die(_)
            | Stmt::Become { .. }
            | Stmt::Transfer { .. }
            | Stmt::Split { .. }
            | Stmt::Join(..)
            | Stmt::Harvest { .. }
            | Stmt::Move(..)
            | Stmt::Dig(..)
            | Stmt::Build { .. }
            | Stmt::Drink(..)
            | Stmt::Eat(..)
            | Stmt::Hit(..)
            | Stmt::Graze(..)
            | Stmt::Look(_)
            | Stmt::Signal(_)
            | Stmt::Grow(..)
            | Stmt::Add(..)
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
    solid: bool,
    cadence_shift: Option<u8>,
    sight: Option<u8>,
    fuel: Option<u32>,
    food: Option<i32>,
    bite: Option<u8>,
    reach: Option<u8>,
    /// The most a step may rise, and in which field.
    climb: Option<(i32, u8)>,
    /// A declared layer (2 and up).
    layer: Option<u8>,
    /// `group max N init M`: the most heads, and the heads a row starts with.
    group: Option<(u16, u16)>,
    /// `size W H`.
    size: Option<(u8, u8)>,
    tags: Vec<String>,
    needs: Vec<NeedDef>,
    mems: Vec<String>,
    /// Each gene is also in `mems`.
    genes: Vec<GeneInst>,
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

fn gcd(a: u64, b: u64) -> u64 {
    if b == 0 { a } else { gcd(b, a % b) }
}

/// The `n` terms of `c`'s top-level `and` as the parser chained them (`a
/// and b and c` is `And(And(a, b), c)`; a parenthesised `and` stays one
/// term); fewer if `c` has fewer.
fn conjuncts(c: &Cond, n: usize) -> Vec<&Cond> {
    let mut parts = Vec::new();
    let mut c = c;
    while parts.len() + 1 < n
        && let Cond::And(l, r) = c
    {
        parts.push(r.as_ref());
        c = l;
    }
    parts.push(c);
    parts.reverse();
    parts
}

/// The source of `text` from one (line, column) to another (both from 1,
/// columns in characters), comments dropped, on one line.
fn source_between(text: &str, from: (u32, u32), to: (u32, u32)) -> String {
    let byte = |line: &str, col: u32| {
        line.char_indices()
            .nth(col.saturating_sub(1) as usize)
            .map_or(line.len(), |(b, _)| b)
    };
    let mut words = Vec::new();
    for (n, line) in (1u32..).zip(text.lines()) {
        if n < from.0 || n > to.0 {
            continue;
        }
        let a = if n == from.0 { byte(line, from.1) } else { 0 };
        let b = if n == to.0 {
            byte(line, to.1)
        } else {
            line.len()
        };
        let part = line.get(a..b.max(a)).unwrap_or("");
        words.extend(part.split('#').next().unwrap_or("").split_whitespace());
    }
    words.join(" ")
}

/// The first statement of `body` that ends the think, as `wmc rates` names
/// it (`RuleShape::acts`); `None` when none does.
fn acts(body: &[Stmt]) -> Option<String> {
    body.iter().find_map(|s| {
        Some(match s {
            Stmt::Die(_) => "die".into(),
            Stmt::Become { kind, .. } => format!("become {kind}"),
            Stmt::Spawn { kind, .. } => format!("spawn {kind}"),
            Stmt::Move(..) => "move".into(),
            Stmt::Drink(..) => "drink".into(),
            Stmt::Eat(..) => "eat".into(),
            Stmt::Hit(..) => "hit".into(),
            Stmt::Graze(..) => "graze".into(),
            Stmt::Transfer { give: true, .. } => "give".into(),
            Stmt::Transfer { give: false, .. } => "take".into(),
            Stmt::Split { .. } => "split".into(),
            Stmt::Join(..) => "join".into(),
            Stmt::Idle(_) => "idle".into(),
            Stmt::Next(state, _) => format!("next {state}"),
            Stmt::Call { name, .. } => format!("{name}()"),
            Stmt::If { then, els, .. } if acts(then).is_some() || acts(els).is_some() => {
                "if".into()
            }
            Stmt::Choose(arms) if arms.iter().any(|(_, b)| acts(b).is_some()) => "choose".into(),
            _ => return None,
        })
    })
}

struct Gen<'a> {
    items: &'a [ItemAst],
    subs: &'a [SubAst],
    consts: &'a [ConstAst],
    /// Constants set from outside the rules: they replace the declared value.
    sets: &'a [ConstSet],
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
    lenient: std::collections::HashSet<usize>,
    /// The instances of each (item, arguments), in the order made.
    inst_memo: HashMap<(usize, Vec<i32>), Vec<usize>>,
    /// Each item's index by name (the first, if two share one).
    item_index: HashMap<&'a str, usize>,
    locals: Vec<Local>,
    next_local: u8,
    /// Position for errors without a better one.
    here: Pos,
    /// Tag names, in first-appearance order (item order, then declaration
    /// order): a tag's bit is its index here.
    tags: Vec<String>,
    /// The `field` declarations, and what they resolve to (same order).
    fields_ast: &'a [FieldAst],
    /// The arena's `budget`, as written, and folded: fuel, rows, private.
    budget_ast: Option<&'a BudgetAst>,
    /// The players' names: a kind `PLAYER.NAME` is theirs (a namespace's
    /// `SPACE.NAME` is the arena's).
    player_list: Vec<String>,
    budget: [Option<u32>; 3],
    field_defs: Vec<FieldDef>,
    /// The `ground` and `feature` declarations, and the materials they make
    /// with the engine's three.
    materials_ast: &'a [MaterialAst],
    materials: Materials,
    /// The `layer` declarations.
    items_ast_layers: &'a [(LayerDef, Pos, Option<(String, Pos)>)],
    /// What each declared layer rests on, resolved (`LayerDef::on`).
    layer_on: Vec<Option<i32>>,
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
    acts_memo: RefCell<HashMap<(usize, Option<usize>, u32, Reach), bool>>,
    /// Compiling a `when` condition: a sub called here must not act.
    in_when: bool,
    /// Compiling a search's `next`: its state's first local, width and
    /// guess, for a `try`.
    in_next: Option<(u8, u8, Option<Expr>, usize)>,
}

/// Does this statement emit an action? Its position, if so.
/// What [`Gen::call_acts`] and its kin look for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Reach {
    /// An action or a `next`.
    Act,
    /// A search started.
    Search,
}

fn action_at(s: &Stmt) -> Option<&Pos> {
    match s {
        Stmt::Idle(at)
        | Stmt::Die(at)
        | Stmt::Become { at, .. }
        | Stmt::Spawn { pos: at, .. }
        | Stmt::Transfer { at, .. }
        | Stmt::Split { at, .. }
        | Stmt::Join(_, at)
        | Stmt::Harvest { at, .. }
        | Stmt::Move(_, at)
        | Stmt::Dig(_, at)
        | Stmt::Build { at, .. }
        | Stmt::Drink(_, at)
        | Stmt::Eat(_, at)
        | Stmt::Hit(_, at)
        | Stmt::Graze(_, at) => Some(at),
        Stmt::Search(_)
        | Stmt::Try { .. }
        | Stmt::Set { .. }
        | Stmt::SetAt { .. }
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
        | Stmt::Grow(..)
        | Stmt::Add(..)
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
        // A search's `next` may not return (the compiler refuses it).
        Stmt::Search(_)
        | Stmt::Try { .. }
        | Stmt::Set { .. }
        | Stmt::SetAt { .. }
        | Stmt::Assign { .. }
        | Stmt::Let { .. }
        | Stmt::Call { .. }
        | Stmt::Idle(_)
        | Stmt::Die(_)
        | Stmt::Become { .. }
        | Stmt::Spawn { .. }
        | Stmt::Transfer { .. }
        | Stmt::Split { .. }
        | Stmt::Join(..)
        | Stmt::Harvest { .. }
        | Stmt::Move(..)
        | Stmt::Dig(..)
        | Stmt::Build { .. }
        | Stmt::Drink(..)
        | Stmt::Eat(..)
        | Stmt::Hit(..)
        | Stmt::Graze(..)
        | Stmt::Look(_)
        | Stmt::Signal(_)
        | Stmt::Grow(..)
        | Stmt::Add(..)
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
    fn new(items: &'a Items, files: &'a [(&'a str, &'a str)], sets: &'a [ConstSet]) -> Self {
        Self {
            files,
            sets,
            debug: DebugInfo {
                files: files.iter().map(|(n, _)| n.to_string()).collect(),
                ..DebugInfo::default()
            },
            state: None,
            ends_memo: RefCell::default(),
            acts_memo: RefCell::default(),
            in_when: false,
            in_next: None,
            items: &items.items,
            subs: &items.subs,
            consts: &items.consts,
            fields_ast: &items.fields,
            budget_ast: items.budgets.first(),
            player_list: Vec::new(),
            budget: [None; 3],
            field_defs: Vec::new(),
            materials_ast: &items.materials,
            materials: Materials::default(),
            items_ast_layers: &items.layers,
            layer_on: Vec::new(),
            const_vals: Vec::new(),
            insts: Vec::new(),
            kind_insts: Vec::new(),
            item_ids: Vec::new(),
            member_index: Vec::new(),
            tags: Vec::new(),
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
            lenient: std::collections::HashSet::new(),
            inst_memo: HashMap::new(),
            item_index: items
                .items
                .iter()
                .enumerate()
                .rev()
                .map(|(i, it)| (it.name.as_str(), i))
                .collect(),
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
        self.item_index.get(name).copied()
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

    /// What each layer rests on (`layer NAME { on KIND }`): a kind or a tag,
    /// once the kinds are numbered. Whether the kind stands is checked with
    /// the kind table ([`Gen::check_rests`]).
    fn resolve_rests(&mut self) -> Result<()> {
        let mut out = Vec::with_capacity(self.items_ast_layers.len());
        for (_, _, on) in self.items_ast_layers {
            out.push(match on {
                None => None,
                Some((name, at)) => {
                    let p = self.named_pred(name, false, at)?;
                    if p < 0 {
                        return Err(self.err(
                            at,
                            format!(
                                "`on {name}`: a layer rests on a kind or a tag, not a material"
                            ),
                        ));
                    }
                    Some(p)
                }
            });
        }
        self.layer_on = out;
        Ok(())
    }

    /// A layer rests on one that stands: not on ground cover, nor on a
    /// kind in a declared layer.
    fn check_rests(&self, defs: &[KindDef]) -> Result<()> {
        for ((l, _, on), p) in self.items_ast_layers.iter().zip(&self.layer_on) {
            let (Some((name, at)), Some(p)) = (on, p) else {
                continue;
            };
            if let Some(d) = usize::try_from(*p).ok().and_then(|k| defs.get(k))
                && d.layer != 0
            {
                return Err(self.err(
                    at,
                    format!(
                        "layer `{}` rests on `{name}`, which does not stand (it lives in another layer)",
                        l.name
                    ),
                ));
            }
        }
        Ok(())
    }

    /// Layers: global names like a kind's; at most [`MAX_LAYERS`].
    fn resolve_layers(&self) -> Result<()> {
        if let Some((_, at, _)) = self.items_ast_layers.get(MAX_LAYERS) {
            return Err(self.err(at, format!("at most {MAX_LAYERS} layers in a rule set")));
        }
        for (i, (l, at, _)) in self.items_ast_layers.iter().enumerate() {
            let taken = self.items_ast_layers[..i]
                .iter()
                .any(|(o, ..)| o.name == l.name)
                || self.item_named(&l.name).is_some()
                || self.subs.iter().any(|s| s.name == l.name)
                || self.tags.contains(&l.name)
                || self.consts.iter().any(|c| c.name == l.name)
                || self.materials.ground(&l.name).is_some()
                || self.materials.feature(&l.name).is_some()
                || ["standing", "cover"].contains(&l.name.as_str());
            if taken {
                return Err(self.err(
                    at,
                    format!("`{}` is already a name: a layer takes its own", l.name),
                ));
            }
        }
        Ok(())
    }

    /// Grounds and features: the engine's soil, water and rock first, each
    /// redeclared in place (its properties as written), then the new ones in
    /// declaration order. Names are global: never a kind's, a trait's, a
    /// sub's, a tag's, a constant's or another material's.
    fn resolve_materials(&mut self) -> Result<()> {
        let mut m = Materials::default();
        for (i, a) in self.materials_ast.iter().enumerate() {
            let taken = if let Some(o) = self.materials_ast[..i].iter().find(|o| o.name == a.name) {
                Some(format!(
                    "`{}` declared twice (first at {}:{})",
                    a.name, o.at.file, o.at.line
                ))
            } else if let Some(o) = self.item_named(&a.name) {
                let what = if self.items[o].is_trait {
                    "trait"
                } else {
                    "kind"
                };
                Some(format!("`{}` is already a {what}", a.name))
            } else if self.subs.iter().any(|s| s.name == a.name) {
                Some(format!("`{}` is already a sub", a.name))
            } else if self.tags.contains(&a.name) {
                Some(format!("`{}` is already a tag", a.name))
            } else if self.consts.iter().any(|c| c.name == a.name) {
                Some(format!("`{}` is already a const", a.name))
            } else if (a.ground && m.feature(&a.name).is_some())
                || (!a.ground && m.ground(&a.name).is_some())
            {
                Some(format!(
                    "`{}` is a ground and a feature: one name, one material",
                    a.name
                ))
            } else {
                None
            };
            if let Some(msg) = taken {
                return Err(self.err(&a.at, msg));
            }
            let def = MaterialDef {
                name: a.name.clone(),
                glyph: a.glyph.unwrap_or(if a.ground { b'.' } else { b'#' }),
                color: a.color,
                solid: a.solid,
                drink: a.drink,
                opaque: a.opaque,
            };
            let (list, most) = if a.ground {
                (&mut m.grounds, MAX_GROUNDS)
            } else {
                (&mut m.features, MAX_FEATURES)
            };
            match list.iter().position(|d| d.name == a.name) {
                Some(j) => list[j] = def,
                None if list.len() == most => {
                    let what = if a.ground { "grounds" } else { "features" };
                    return Err(self.err(
                        &a.at,
                        format!("at most {most} {what} in a rule set, the engine's own among them"),
                    ));
                }
                None => list.push(def),
            }
        }
        m.table = Materials::table_of(&m.grounds, &m.features);
        self.materials = m;
        Ok(())
    }

    /// Fields: global names (never a kind's, a trait's, a sub's, a tag's or
    /// a constant's; a need may share one), their numbers folded.
    fn resolve_fields(&mut self) -> Result<()> {
        if let Some(f) = self.fields_ast.get(MAX_FIELDS) {
            return Err(self.err(&f.at, format!("at most {MAX_FIELDS} fields in a rule set")));
        }
        for (i, f) in self.fields_ast.iter().enumerate() {
            let first = |at: &Pos| format!(" (first at {}:{})", at.file, at.line);
            let taken = if let Some(o) = self.fields_ast[..i].iter().find(|o| o.name == f.name) {
                Some(format!("field `{}` declared twice{}", f.name, first(&o.at)))
            } else if let Some(o) = self.item_named(&f.name) {
                let what = if self.items[o].is_trait {
                    "trait"
                } else {
                    "kind"
                };
                Some(format!(
                    "`{}` is already a {what}{}",
                    f.name,
                    first(&self.items[o].at)
                ))
            } else if let Some(o) = self.subs.iter().find(|s| s.name == f.name) {
                Some(format!("`{}` is already a sub{}", f.name, first(&o.at)))
            } else if self.tags.contains(&f.name) {
                Some(format!("`{}` is already a tag", f.name))
            } else if let Some(o) = self.consts.iter().find(|c| c.name == f.name) {
                Some(format!("`{}` is already a const{}", f.name, first(&o.at)))
            } else {
                self.items
                    .iter()
                    .find(|it| it.members.iter().any(|m| m.name == f.name))
                    .map(|it| format!("`{}` is already a sub of `{}`", f.name, it.name))
            };
            if let Some(msg) = taken {
                return Err(self.err(&f.at, msg));
            }
            let max = self.fold(&f.max, &f.at)?;
            let max = u16::try_from(max).ok().filter(|&m| m > 0).ok_or_else(|| {
                self.err(
                    &f.at,
                    format!("field `{}`: max {max} is not in 1..=65535", f.name),
                )
            })?;
            let init =
                match &f.init {
                    None => FieldInit::Value(0),
                    Some((false, e, at)) => {
                        let v = self.fold(e, at)?;
                        FieldInit::Value(
                            u16::try_from(v).ok().filter(|&v| v <= max).ok_or_else(|| {
                                self.err(at, format!("init {v} is not in 0..={max}"))
                            })?,
                        )
                    }
                    Some((true, e, at)) => {
                        let v = self.fold(e, at)?;
                        FieldInit::Noise(u32::try_from(v).ok().filter(|&v| v > 0).ok_or_else(
                            || self.err(at, format!("noise scale {v} is not above 0")),
                        )?)
                    }
                };
            let outside =
                match &f.outside {
                    None => None,
                    Some((e, at)) => {
                        let v = self.fold(e, at)?;
                        Some(u16::try_from(v).ok().filter(|&v| v <= max).ok_or_else(|| {
                            self.err(at, format!("outside {v} is not in 0..={max}"))
                        })?)
                    }
                };
            self.field_defs.push(FieldDef {
                name: f.name.clone(),
                max,
                policy: f.policy,
                init,
                color: f.color,
                outside,
                law: None,
                relief: f.relief,
            });
        }
        Ok(())
    }

    /// The arena's `budget`, folded: `fuel` 1 to [`MAX_FUEL`], `rows` and
    /// `private` 0 or more. A player's private fields are counted here.
    fn resolve_budget(&mut self) -> Result<()> {
        let Some(b) = self.budget_ast else {
            return Ok(());
        };
        let parts = [
            (&b.fuel, 1, MAX_FUEL),
            (&b.rows, 0, u32::MAX),
            (&b.private, 0, u32::MAX),
        ];
        for (i, (part, lo, hi)) in parts.into_iter().enumerate() {
            let Some((e, at)) = part else {
                continue;
            };
            let v = self.fold(e, at)?;
            let v = u32::try_from(v)
                .ok()
                .filter(|v| (lo..=hi).contains(v))
                .ok_or_else(|| {
                    let what = ["fuel", "rows", "private"][i];
                    self.err(at, format!("budget {what} {v}: {lo} to {hi}"))
                })?;
            self.budget[i] = Some(v);
        }
        if let Some(cap) = self.budget[2] {
            let mut seen: Vec<(&str, u32)> = Vec::new();
            for f in self.fields_ast.iter().filter(|f| f.private) {
                let player = f.name.split('.').next().unwrap_or_default();
                let n = match seen.iter_mut().find(|(p, _)| *p == player) {
                    Some((_, n)) => n,
                    None => {
                        seen.push((player, 0));
                        &mut seen.last_mut().expect("just pushed").1
                    }
                };
                *n += 1;
                if *n > cap {
                    return Err(self.err(
                        &f.at,
                        format!("player `{player}` declares more private fields than the arena's budget of {cap}"),
                    ));
                }
            }
        }
        Ok(())
    }

    /// Who owns which kind: a player's kinds are named `PLAYER.NAME`. With
    /// the budget's `fuel`, a player's kind thinks in at most that many ops.
    fn players(&self, defs: &mut [KindDef]) -> Players {
        let mut p = Players {
            rows: self.budget[1],
            ..Players::default()
        };
        for d in defs.iter_mut() {
            let Some((player, _)) = d
                .name
                .split_once('.')
                .filter(|(o, _)| self.player_list.iter().any(|p| p == o))
            else {
                p.of.push(Players::NONE);
                continue;
            };
            let i = match p.names.iter().position(|n| n == player) {
                Some(i) => i,
                None => {
                    p.names.push(player.to_string());
                    p.names.len() - 1
                }
            };
            p.of.push(u8::try_from(i).expect("at most MAX_PLAYERS, checked in compile_owned"));
            if let Some(cap) = self.budget[0] {
                d.fuel = d.fuel.min(cap);
            }
        }
        if p.names.is_empty() && p.rows.is_none() {
            return Players::default();
        }
        p
    }

    /// Every field's law, once the kinds are numbered: a law reads any
    /// field, declared before it or after, and any kind (`is(here, hen)`).
    fn resolve_laws(&mut self) -> Result<()> {
        for (i, f) in self.fields_ast.iter().enumerate() {
            if let Some((every, body, at)) = &f.law {
                let law = self.law(i, every, body, at)?;
                self.field_defs[i].law = Some(law);
            }
        }
        Ok(())
    }

    /// Field `name`'s index and definition.
    fn field_named(&self, name: &str) -> Option<(u8, FieldDef)> {
        let i = self.field_defs.iter().position(|f| f.name == name)?;
        Some((
            u8::try_from(i).expect("at most MAX_FIELDS fields"),
            self.field_defs[i].clone(),
        ))
    }

    /// [`Self::field_named`], or an error naming the fields there are.
    fn field(&self, name: &str, at: &Pos) -> Result<(u8, FieldDef)> {
        self.field_named(name).ok_or_else(|| {
            let names: Vec<&str> = self.field_defs.iter().map(|f| f.name.as_str()).collect();
            self.err(
                at,
                if names.is_empty() {
                    format!("no field `{name}`: the rules declare no field")
                } else {
                    format!("no field `{name}` (the fields: {})", names.join(", "))
                },
            )
        })
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
                    // A layer that rests on a kind takes a tag bit of its own,
                    // by its place among the layers (`vm::support_bit`):
                    // the bits from the top down to the last resting one's.
                    let resting = self
                        .items_ast_layers
                        .iter()
                        .rposition(|l| l.2.is_some())
                        .map_or(0, |i| i + 1);
                    if self.tags.len() == MAX_TAGS - resting {
                        let why = if resting > 0 {
                            format!(
                                " ({MAX_TAGS}, less one for each layer up to the last that rests on a kind)"
                            )
                        } else {
                            String::new()
                        };
                        return Err(self.err(
                            &it.at,
                            format!("at most {} tags in a rule set{why}", MAX_TAGS - resting),
                        ));
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
            let v = match self.sets.iter().find(|(n, _)| *n == c.name) {
                Some(&(_, v)) => v,
                None => self.fold(&c.value, &c.at)?,
            };
            self.const_vals.push((c.name.clone(), v));
        }
        if let Some((name, _)) = self
            .sets
            .iter()
            .find(|(n, _)| !self.consts.iter().any(|c| c.name == *n))
        {
            let mut declared: Vec<&str> = self.consts.iter().map(|c| c.name.as_str()).collect();
            declared.sort_unstable();
            return Err(CompileError {
                file: "--set".into(),
                line: 0,
                col: 0,
                msg: format!(
                    "`{name}`: the rules declare no such const (they declare: {})",
                    if declared.is_empty() {
                        "none".into()
                    } else {
                        declared.join(", ")
                    }
                ),
            });
        }
        self.resolve_budget()?;
        self.resolve_materials()?;
        self.resolve_layers()?;
        self.resolve_fields()?;
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
            // A slot named like a const would read as the slot in code and
            // as the const where only consts are known (`wmc rates`).
            let needs = it
                .decls
                .needs
                .iter()
                .map(|n| ("need", n.name.as_str(), &n.at));
            let mems = it
                .decls
                .mems
                .iter()
                .map(|(m, at)| ("mem", array_of(m).unwrap_or(m), at));
            let genes = it
                .decls
                .genes
                .iter()
                .map(|g| ("gene", g.name.as_str(), &g.at));
            for (what, n, at) in needs.chain(mems).chain(genes) {
                if self.const_value(n).is_some() {
                    return Err(self.err(
                        at,
                        format!(
                            "{what} `{n}` of `{}` has the name of the constant `{n}`",
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
        self.resolve_laws()?;
        self.resolve_rests()?;
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
        self.check_rests(&defs)?;
        let players = self.players(&mut defs);
        let kinds = Kinds::from_parts(defs, code, std::mem::take(&mut self.pool), sub_entries)
            .with_fields(std::mem::take(&mut self.field_defs))
            .with_materials(std::mem::take(&mut self.materials))
            .with_layers(
                self.items_ast_layers
                    .iter()
                    .zip(&self.layer_on)
                    .map(|((l, ..), &on)| LayerDef { on, ..l.clone() })
                    .collect(),
            )
            .with_players(players);
        self.debug.diagnostics = self.lint(&kinds);
        Ok(kinds.with_debug(std::mem::take(&mut self.debug)))
    }

    // ---- inheritance ------------------------------------------------------------------

    /// Resolve `item` with `args` (a trait's arguments; none for a kind):
    /// its ancestors, merged declarations and rule lists. Memoized per
    /// (item, arguments).
    fn inst(&mut self, item: usize, args: Vec<i32>, stack: &mut Vec<usize>) -> Result<usize> {
        if let Some(&i) = self.inst_memo.get(&(item, args.clone())).and_then(|v| {
            v.iter()
                .find(|&&i| self.placeholder || !self.lenient.contains(&i))
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
        self.inst_memo
            .entry((item, inst.args.clone()))
            .or_default()
            .push(me);
        self.insts.push(inst);
        if skipped {
            self.lenient.insert(me);
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
        let reach = self
            .decl_num(
                &d.reach,
                |r| (1..=i32::from(MAX_SIGHT)).contains(&r),
                &format!("reach is 1 to {MAX_SIGHT} cells"),
            )?
            .map(|r| u8::try_from(r).expect("1..=MAX_SIGHT, checked above"));
        let reach = self.pick(reach, &parents, |i| i.reach, "reaches", item)?;
        let climb = match &d.climb {
            None => None,
            Some((n, f, at)) => {
                let n = self.fold(n, at)?;
                let field = self.field(f, at)?.0;
                // Checking a trait, out of range is "not declared".
                if n < 0 && !self.checking {
                    return Err(self.err(at, format!("climb {n}: a step rises 0 or more")));
                }
                (n >= 0).then_some((n, field))
            }
        };
        let climb = self.pick(climb, &parents, |i| i.climb, "climbs", item)?;
        let layer = match &d.layer {
            None => None,
            Some((name, at)) => {
                let i = self
                    .items_ast_layers
                    .iter()
                    .position(|(l, ..)| l.name == *name);
                let i = i.ok_or_else(|| {
                    self.err(
                        at,
                        format!("no layer `{name}`: declare it with `layer {name}`"),
                    )
                })?;
                Some(u8::try_from(i + 2).expect("at most MAX_LAYERS layers"))
            }
        };
        let layer = self.pick(layer, &parents, |i| i.layer, "layers", item)?;
        let cover = d.cover || parents.iter().any(|&p| self.insts[p].cover);
        if cover && let Some(l) = layer {
            let name = &self.items_ast_layers[usize::from(l) - 2].0.name;
            return Err(self.err(
                &it.at,
                format!(
                    "`{}` is ground cover and in layer `{name}`: a kind lives in one layer",
                    it.name
                ),
            ));
        }
        let solid = d.solid || parents.iter().any(|&p| self.insts[p].solid);
        let group = match &d.group {
            None => None,
            Some((max, init, at)) => {
                let max = self.fold(max, at)?;
                let init = match init {
                    Some(e) => self.fold(e, at)?,
                    None => 1,
                };
                let fits = (2..=i32::from(u16::MAX)).contains(&max) && (1..=max).contains(&init);
                if !fits && self.checking {
                    None
                } else if !(2..=i32::from(u16::MAX)).contains(&max) {
                    return Err(self.err(
                        at,
                        format!("group max {max}: a group holds 2 to 65535 heads"),
                    ));
                } else if !(1..=max).contains(&init) {
                    return Err(self.err(
                        at,
                        format!(
                            "group init {init}: a group starts with 1 to its max ({max}) heads"
                        ),
                    ));
                } else {
                    Some((
                        u16::try_from(max).expect("2..=65535, checked above"),
                        u16::try_from(init).expect("1..=max, checked above"),
                    ))
                }
            }
        };
        let group = self.pick(group, &parents, |i| i.group, "groups", item)?;
        let size = match &d.size {
            None => None,
            Some((w, h, at)) => {
                let (w, h) = (self.fold(w, at)?, self.fold(h, at)?);
                if !(1..=MAX_SIZE).contains(&w) || !(1..=MAX_SIZE).contains(&h) {
                    if !self.checking {
                        return Err(
                            self.err(at, format!("size {w} {h}: 1 to {MAX_SIZE} cells each way"))
                        );
                    }
                    None
                } else {
                    Some((
                        u8::try_from(w).expect("1..=16, checked above"),
                        u8::try_from(h).expect("1..=16, checked above"),
                    ))
                }
            }
        };
        let size = self.pick(size, &parents, |i| i.size, "sizes", item)?;
        if size.is_some_and(|s| s != (1, 1)) {
            if cover {
                return Err(self.err(
                    &it.at,
                    format!(
                        "`{}` is ground cover and bigger than a cell: ground cover is one a cell",
                        it.name
                    ),
                ));
            }
            if let Some(l) = layer
                && self.items_ast_layers[usize::from(l) - 2].2.is_some()
            {
                let name = &self.items_ast_layers[usize::from(l) - 2].0.name;
                return Err(self.err(
                    &it.at,
                    format!("`{}` is bigger than a cell and lives in `{name}`, which rests on a kind: one cell, one support", it.name),
                ));
            }
        }
        if cover && group.is_some() {
            return Err(self.err(
                &it.at,
                format!(
                    "`{}` is ground cover and a group: ground cover is one a cell",
                    it.name
                ),
            ));
        }

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
        // Genes by name like needs: the item's own override its parents'.
        let mut genes: Vec<GeneInst> = Vec::new();
        let mut gene_from: Vec<usize> = Vec::new();
        let mut gene_clashes: Vec<(String, usize, usize)> = Vec::new();
        for &p in &parents {
            for g in &self.insts[p].genes {
                match genes.iter().position(|x| x.name == g.name) {
                    None => {
                        genes.push(g.clone());
                        gene_from.push(p);
                    }
                    Some(i) if genes[i] == *g => {}
                    Some(i) => gene_clashes.push((g.name.clone(), gene_from[i], p)),
                }
            }
        }
        for ga in &d.genes {
            let (mut default, lo, mut hi) = (
                self.fold(&ga.default, &ga.at)?,
                self.fold(&ga.lo, &ga.at)?,
                self.fold(&ga.hi, &ga.at)?,
            );
            let mut mutate = match &ga.mutate {
                Some(e) => self.fold(e, &ga.at)?,
                None => 0,
            };
            // Traits are checked with every parameter 1: lenient there.
            if self.checking {
                hi = hi.max(lo);
                default = default.clamp(lo, hi);
                mutate = mutate.clamp(0, 100);
            }
            if lo > hi || default < lo || default > hi {
                return Err(self.err(
                    &ga.at,
                    format!(
                        "gene `{}`: the range is `from LO to HI` with LO <= DEFAULT <= HI (got {default} from {lo} to {hi})",
                        ga.name
                    ),
                ));
            }
            if !(0..=100).contains(&mutate) {
                return Err(self.err(
                    &ga.at,
                    format!(
                        "gene `{}`: `mutate` is a percent, 0 to 100 (got {mutate})",
                        ga.name
                    ),
                ));
            }
            let def = GeneInst {
                name: ga.name.clone(),
                default,
                lo,
                hi,
                mutate: u8::try_from(mutate).expect("checked 0..=100"),
            };
            match genes.iter().position(|x| x.name == def.name) {
                Some(i) => genes[i] = def,
                None => genes.push(def),
            }
        }
        if let Some((n, a, b)) = gene_clashes
            .iter()
            .find(|(n, ..)| !d.genes.iter().any(|x| x.name == *n))
        {
            return Err(self.err(
                &it.at,
                format!(
                    "`{}` inherits gene `{n}` from `{}` and `{}`, declared differently: redeclare it in `{}`",
                    it.name,
                    self.inst_name(*a),
                    self.inst_name(*b),
                    it.name
                ),
            ));
        }
        // An array has one length: a trait's and its kind's, two parents'.
        let mut lengths: Vec<(&str, usize, &str)> = Vec::new();
        let sources = parents
            .iter()
            .map(|&p| {
                (
                    self.inst_name(p),
                    self.insts[p].mems.iter().map(String::as_str).collect(),
                )
            })
            .chain([(
                it.name.as_str(),
                d.mems.iter().map(|(m, _)| m.as_str()).collect(),
            )]);
        for (owner, list) in sources {
            let list: Vec<&str> = list;
            let mut seen: Vec<(&str, usize)> = Vec::new();
            for a in list.iter().filter_map(|m| array_of(m)) {
                match seen.iter_mut().find(|(n, _)| *n == a) {
                    Some((_, k)) => *k += 1,
                    None => seen.push((a, 1)),
                }
            }
            for (a, k) in seen {
                match lengths.iter().find(|(n, ..)| *n == a) {
                    Some(&(_, j, other)) if j != k => {
                        return Err(self.err(
                            &it.at,
                            format!(
                                "`{a}` is declared with two lengths, {j} in `{other}` and {k} in `{owner}`: declare the array once"
                            ),
                        ));
                    }
                    Some(_) => {}
                    None => lengths.push((a, k, owner)),
                }
            }
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
            if genes.iter().any(|g| g.name == *m) {
                return Err(self.err(at, format!("`{m}` is a gene of `{}` already", it.name)));
            }
            if !mems.contains(m) {
                mems.push(m.clone());
            }
        }
        // A gene is a mem slot nobody writes; it takes the end of the list.
        for g in &genes {
            if mems.contains(&g.name) {
                let plain = parents.iter().any(|&p| {
                    self.insts[p].mems.contains(&g.name)
                        && !self.insts[p].genes.iter().any(|x| x.name == g.name)
                });
                if plain {
                    return Err(self.err(
                        &it.at,
                        format!(
                            "`{}` inherits `{}` both as a gene and as a mem slot",
                            it.name, g.name
                        ),
                    ));
                }
            } else {
                mems.push(g.name.clone());
            }
            if needs.iter().any(|n| n.name == g.name) {
                return Err(self.err(
                    &it.at,
                    format!(
                        "`{}` has `{}` both as a need and as a gene",
                        it.name, g.name
                    ),
                ));
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
                if genes.iter().any(|g| g.name == *name) {
                    return Err(self.err(
                        at,
                        format!(
                            "`spawn {} ... with`: `{name}` is a gene, inherited and never set",
                            it.name
                        ),
                    ));
                }
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
            solid,
            cadence_shift,
            sight,
            fuel,
            food,
            bite,
            reach,
            climb,
            layer,
            group,
            size,
            tags,
            needs,
            mems,
            genes,
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
        // Each kind's concrete parent, then each kind's children, once.
        let parent: Vec<Option<usize>> = (0..items.len())
            .map(|i| {
                items[i]
                    .parents
                    .iter()
                    .find_map(|p| self.item_named(&p.name).filter(|&pi| !items[pi].is_trait))
            })
            .collect();
        let kinds: Vec<usize> = (0..items.len()).filter(|&i| !items[i].is_trait).collect();
        let mut children: Vec<Vec<usize>> = vec![Vec::new(); items.len()];
        for &k in kinds.iter().rev() {
            if let Some(p) = parent[k] {
                children[p].push(k);
            }
        }
        let mut order = Vec::with_capacity(kinds.len());
        let mut stack: Vec<usize> = kinds
            .iter()
            .rev()
            .copied()
            .filter(|&k| parent[k].is_none())
            .collect();
        while let Some(k) = stack.pop() {
            order.push(k);
            stack.extend_from_slice(&children[k]);
        }
        self.item_ids = vec![None; items.len()];
        for (id, &k) in order.iter().enumerate() {
            self.item_ids[k] = Some(u16::try_from(id).expect("checked in generate"));
        }
        self.kind_insts = order
            .iter()
            .map(|&k| {
                self.inst_memo
                    .get(&(k, Vec::new()))
                    .and_then(|v| v.first().copied())
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
            reach: inst.reach.unwrap_or(1),
            climb: inst.climb.map(|(n, f)| (f, n)),
            needs: inst.needs.clone(),
            mems: inst.mems.clone(),
            genes: inst
                .genes
                .iter()
                .map(|g| GeneDef {
                    slot: u8::try_from(
                        inst.mems
                            .iter()
                            .position(|m| *m == g.name)
                            .expect("in mems"),
                    )
                    .expect("at most MEM_SLOTS mems"),
                    default: g.default,
                    lo: g.lo,
                    hi: g.hi,
                    mutate: g.mutate,
                })
                .collect(),
            states: u8::try_from(inst.states.len().max(1)).expect("at most MAX_STATES states"),
            entry,
            color: inst.color.unwrap_or(DEFAULT_COLOR),
            cover: inst.cover,
            layer: if inst.cover {
                1
            } else {
                inst.layer.unwrap_or(0)
            },
            solid: inst.solid,
            parent,
            group: inst.group.unwrap_or((0, 0)),
            size: inst.size.unwrap_or((1, 1)),
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
        let marks = (self.pool.len(), self.debug.rules.len());
        let was = self.checking;
        self.checking = true;
        let r = self.check_all_traits();
        self.asm = saved_asm;
        self.pool.truncate(marks.0);
        self.debug.rules.truncate(marks.1);
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

    /// May a call of `name` here act or `next` (or, `m` = [`Reach::Search`],
    /// start a search), on any path? Follows every call in its body, in
    /// statements and in expressions, eight deep.
    fn call_acts(&self, name: &str, depth: u32, m: Reach) -> bool {
        (depth as usize) < FRAMES
            && self.callee(name).is_some_and(|sub| {
                let key = (std::ptr::from_ref(sub) as usize, self.cur, depth, m);
                if let Some(&v) = self.acts_memo.borrow().get(&key) {
                    return v;
                }
                let v = sub.body.iter().any(|s| self.stmt_acts(s, depth + 1, m));
                self.acts_memo.borrow_mut().insert(key, v);
                v
            })
    }

    fn stmt_acts(&self, s: &Stmt, depth: u32, m: Reach) -> bool {
        let body = |b: &[Stmt]| b.iter().any(|s| self.stmt_acts(s, depth, m));
        match s {
            Stmt::Next(..) => m == Reach::Act,
            _ if action_at(s).is_some() => m == Reach::Act,
            Stmt::Search(_) if m == Reach::Search => true,
            Stmt::Set { value, .. } | Stmt::Assign { value, .. } | Stmt::Let { value, .. } => {
                self.expr_acts(value, depth, m)
            }
            Stmt::SetAt { index, value, .. } => {
                self.expr_acts(index, depth, m) || self.expr_acts(value, depth, m)
            }
            Stmt::Look(e) | Stmt::Signal(e) | Stmt::Grow(e, _) | Stmt::Add(_, e, _) => {
                self.expr_acts(e, depth, m)
            }
            Stmt::Return { value, .. } => {
                value.as_ref().is_some_and(|e| self.expr_acts(e, depth, m))
            }
            Stmt::If { cond, then, els } => {
                self.cond_acts(cond, depth, m) || body(then) || body(els)
            }
            Stmt::While { cond, body: b } => self.cond_acts(cond, depth, m) || body(b),
            Stmt::Repeat { count, body: b } => self.expr_acts(count, depth, m) || body(b),
            Stmt::ForEach {
                pred, r, body: b, ..
            } => self.pred_acts(pred, depth, m) || self.expr_acts(r, depth, m) || body(b),
            Stmt::Choose(arms) => arms
                .iter()
                .any(|(w, b)| self.expr_acts(w, depth, m) || body(b)),
            Stmt::Call { name, args, .. } => {
                self.args_act(args, depth, m) || self.call_acts(name, depth, m)
            }
            Stmt::Search(se) => {
                se.start.iter().any(|e| self.expr_acts(e, depth, m))
                    || body(&se.next)
                    || self.cond_acts(&se.goal, depth, m)
                    || se
                        .guess
                        .as_ref()
                        .is_some_and(|e| self.expr_acts(e, depth, m))
                    || se
                        .limit
                        .as_ref()
                        .is_some_and(|(e, _)| self.expr_acts(e, depth, m))
            }
            Stmt::Try {
                values, cost, via, ..
            } => values
                .iter()
                .chain(cost)
                .chain(via)
                .any(|e| self.expr_acts(e, depth, m)),
            _ => false,
        }
    }

    fn cond_acts(&self, c: &Cond, depth: u32, m: Reach) -> bool {
        match c {
            Cond::Expr(e) => self.expr_acts(e, depth, m),
            Cond::Nearest { pred, r, .. } => {
                self.pred_acts(pred, depth, m) || self.expr_acts(r, depth, m)
            }
            Cond::Best { pred, r, .. } => {
                pred.as_ref().is_some_and(|p| self.pred_acts(p, depth, m))
                    || self.expr_acts(r, depth, m)
            }
            Cond::And(a, b) | Cond::Or(a, b) => {
                self.cond_acts(a, depth, m) || self.cond_acts(b, depth, m)
            }
            Cond::Not(a) => self.cond_acts(a, depth, m),
            Cond::Search { name, args, .. } => {
                self.args_act(args, depth, m) || self.call_acts(name, depth, m)
            }
        }
    }

    fn expr_acts(&self, e: &Expr, depth: u32, m: Reach) -> bool {
        match e {
            Expr::Neg(a) | Expr::Rand(a) | Expr::Chance(a) | Expr::Index(_, a, _) => {
                self.expr_acts(a, depth, m)
            }
            Expr::Count(p, a) => self.pred_acts(p, depth, m) || self.expr_acts(a, depth, m),
            Expr::IsAt(t, p) => self.target_acts(t, depth, m) || self.pred_acts(p, depth, m),
            // Only in a law, which never acts.
            Expr::Agg { .. } | Expr::Flow { .. } => false,
            Expr::Along { target, .. } => self.target_acts(target, depth, m),
            Expr::Sees { target, eye, .. } => {
                self.target_acts(target, depth, m)
                    || eye.as_ref().is_some_and(|e| self.expr_acts(e, depth, m))
            }
            Expr::Bin(_, a, b) => self.expr_acts(a, depth, m) || self.expr_acts(b, depth, m),
            Expr::Fn(_, args) => args.iter().any(|a| self.expr_acts(a, depth, m)),
            Expr::Dist(t)
            | Expr::FreeAt(t)
            | Expr::FitsAt(t)
            | Expr::LookOf(t)
            | Expr::HeadsOf(t)
            | Expr::SignalOf(t, _) => self.target_acts(t, depth, m),
            Expr::Call { name, args, .. } => {
                self.args_act(args, depth, m) || self.call_acts(name, depth, m)
            }
            Expr::Int(_) | Expr::Name(..) | Expr::Field(..) | Expr::Sense(_) => false,
        }
    }

    /// A field condition's values are expressions, computed in the think.
    fn pred_acts(&self, p: &Pred, depth: u32, m: Reach) -> bool {
        match p {
            Pred::Fields(terms, base, _) => {
                terms.iter().any(|(_, _, e, _)| self.expr_acts(e, depth, m))
                    || base.as_ref().is_some_and(|b| self.pred_acts(b, depth, m))
            }
            _ => false,
        }
    }

    fn target_acts(&self, t: &Target, depth: u32, m: Reach) -> bool {
        match t {
            Target::Heading(e) => self.expr_acts(e, depth, m),
            Target::At(a, b) => self.expr_acts(a, depth, m) || self.expr_acts(b, depth, m),
            Target::Toward(t) | Target::Away(t) => self.target_acts(t, depth, m),
            _ => false,
        }
    }

    fn args_act(&self, args: &[Arg], depth: u32, m: Reach) -> bool {
        args.iter().any(|a| match a {
            Arg::Expr(e) => self.expr_acts(e, depth, m),
            Arg::Target(t) => self.target_acts(t, depth, m),
            Arg::Pred(p) => self.pred_acts(p, depth, m),
            Arg::Name(..) => false,
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
        // Its terms one after another, the same code as the whole condition,
        // each one's first op kept (`RuleInfo::terms`).
        let mut parts = conjuncts(&rule.cond, rule.terms.len());
        if parts.len() != rule.terms.len() {
            parts = vec![&rule.cond];
        }
        let mut term_pcs = Vec::with_capacity(parts.len());
        for part in &parts {
            term_pcs.push(self.asm.here());
            self.cond(part, next, true)?;
        }
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
        // From `when` to its last token, or to the end of the line of `=>`
        // if that is sooner; comments dropped, one line.
        let source = self.files.get(file).map_or("", |(_, t)| *t);
        let line_end = (rule.arrow_line.max(rule.at.line), u32::MAX);
        let text = source_between(source, (rule.at.line, rule.at.col), rule.end.min(line_end));
        let spans: Vec<((u32, u32), (u32, u32))> = if term_pcs.len() == rule.terms.len() {
            rule.terms.clone()
        } else {
            let (from, to) = (rule.terms[0].0, rule.terms[rule.terms.len() - 1].1);
            vec![(from, to)]
        };
        let terms = term_pcs
            .iter()
            .zip(&spans)
            .map(|(&pc, &(from, to))| Term {
                pc,
                text: source_between(source, from, to),
            })
            .collect();
        let via = match (self.owner, self.cur) {
            (Some(o), Some(c)) if o != c => Some(self.inst_name(o).to_string()),
            _ => None,
        };
        let shape = self.shape(rule);
        self.debug.rules.push(RuleInfo {
            kind: self.kind.unwrap_or(0),
            state: self.state,
            file: u16::try_from(file).expect("at most MAX_FILES files"),
            line: rule.at.line,
            text,
            cond_pc,
            body_pc,
            terms,
            via,
            shape,
        });
        Ok(())
    }

    /// What `rule` does and how often (`RuleShape`), with this instance's
    /// parameters and the constants folded in.
    fn shape(&self, rule: &Rule) -> RuleShape {
        let mut shape = RuleShape::default();
        let mut terms = Vec::new();
        let mut stack = vec![&rule.cond];
        while let Some(c) = stack.pop() {
            match c {
                Cond::And(a, b) => {
                    stack.push(b);
                    stack.push(a);
                }
                c => terms.push(c),
            }
        }
        // A name a need, mem or binding hides is not the const.
        let folded = |e: &Expr| self.fold_known(e, &rule.at, |_| false).map(i64::from);
        let mut times = |n: i64, d: i64| {
            let (n, d) = (n.clamp(0, d) as u64, d as u64);
            let (num, den) = (
                shape.dice.0.saturating_mul(n),
                shape.dice.1.saturating_mul(d),
            );
            let g = gcd(num, den).max(1);
            shape.dice = (num / g, den / g);
        };
        for t in terms {
            let Cond::Expr(e) = t else {
                shape.gates = shape.gates.saturating_add(1);
                continue;
            };
            match e {
                Expr::Chance(p) if let Some(p) = folded(p) => times(p, 100),
                Expr::Bin(op @ (OpCode::Lt | OpCode::Le), r, k)
                    if let Expr::Rand(n) = r.as_ref()
                        && let (Some(n), Some(k)) = (folded(n), folded(k))
                        && n > 0 =>
                {
                    times(
                        if *op == OpCode::Le {
                            k.saturating_add(1)
                        } else {
                            k
                        },
                        n,
                    );
                }
                Expr::Bin(OpCode::Gt | OpCode::Ge, a, v)
                    if matches!(a.as_ref(), Expr::Sense(Sense::Age))
                        && let Some(v) = self.fold_known(v, &rule.at, |_| false) =>
                {
                    shape.after_age = Some(v);
                }
                e if folded(e).is_some_and(|v| v != 0) => {} // always true
                _ => shape.gates = shape.gates.saturating_add(1),
            }
        }
        shape.acts = acts(&rule.body);
        shape
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

    /// Memory array `n` (`mem n[N]`): its first slot and its length.
    fn mem_array(&self, n: &str, at: &Pos) -> Result<(u8, u8)> {
        let base = self.mem_slot(&format!("{n}[0]")).ok_or_else(|| {
            self.err(
                at,
                format!("`{n}` is not a memory array of this kind (`mem {n}[N]`)"),
            )
        })?;
        let mut len = 1u8;
        while self.mem_slot(&format!("{n}[{len}]")) == Some(base + len) {
            len += 1;
        }
        // Declared twice with two lengths (a trait's, its kind's): the
        // elements of the longer don't follow one another.
        if self.mem_slot(&format!("{n}[{len}]")).is_some() {
            return Err(self.err(
                at,
                format!("`{n}` is declared with two lengths (in a trait and its kind, say): declare the array once"),
            ));
        }
        Ok((base, len))
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
                // A field condition's slots serve the search alone.
                let mark = self.next_local;
                self.pred(pred)?;
                self.expr(r)?;
                self.asm.nearest(slot).jz(on_false);
                self.next_local = mark;
                self.locals.push(Local {
                    name: bind.clone(),
                    slot,
                    ty: Ty::Target,
                });
            }
            Cond::Best {
                field,
                pred,
                r,
                bind,
                at,
            } => {
                if !top {
                    return Err(self.err(
                        at,
                        "`best ... as` must be a top-level conjunct (not under `or` or `not`)",
                    ));
                }
                if self.local(bind).is_some() {
                    return Err(self.err(at, format!("`{bind}` is already bound")));
                }
                self.hides(bind, &format!("binding `{bind}`"), at)?;
                let slot = self.alloc_local(at, 2)?;
                let mark = self.next_local;
                let f = self.field(field, at)?.0;
                self.push_int(i32::from(f))?;
                match pred {
                    Some(p) => self.pred(p)?,
                    None => self.push_int(pred::ANY)?,
                }
                self.expr(r)?;
                self.asm.best(slot).jz(on_false);
                self.next_local = mark;
                self.locals.push(Local {
                    name: bind.clone(),
                    slot,
                    ty: Ty::Target,
                });
            }
            Cond::Search {
                name,
                args,
                bind,
                into,
                at,
            } => {
                if !top {
                    return Err(self.err(
                        at,
                        "`... as plan` must be a top-level conjunct (not under `or` or `not`)",
                    ));
                }
                if self.local(bind).is_some() {
                    return Err(self.err(at, format!("`{bind}` is already bound")));
                }
                self.hides(bind, &format!("binding `{bind}`"), at)?;
                if !self.callee_is_search(name) {
                    return Err(self.err(
                        at,
                        format!("`{name}` is not a search: `as` binds a search's plan"),
                    ));
                }
                let array = match into {
                    Some(a) => Some(self.mem_array(a, at)?),
                    None => None,
                };
                let slot = self.alloc_local(at, 3)?;
                self.call(name, args, at)?;
                self.asm.jz(on_false);
                self.asm.search(OpCode::SearchOut, slot, 0);
                if let Some((b, len)) = array {
                    self.asm.search(OpCode::SearchInto, b, len);
                }
                self.locals.push(Local {
                    name: bind.clone(),
                    slot,
                    ty: Ty::Plan,
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

    /// A kind (its family, or `only` it), a tag or a material by name, as
    /// a predicate value.
    /// `grow`, `split` and `join` are a group's: in a kind's code (not a
    /// file sub's, which has no kind), and the kind a group.
    fn group_only(&self, verb: &str, at: &Pos) -> Result<()> {
        let Some(i) = self.cur else {
            return Err(self.err(at, format!("`{verb}` inside a sub: heads belong to a kind")));
        };
        if verb != "grow" && self.insts[i].size.is_some_and(|s| s != (1, 1)) {
            return Err(self.err(
                at,
                format!(
                    "`{verb}`: `{}` is bigger than a cell, and a big group neither splits nor joins",
                    self.inst_name(i)
                ),
            ));
        }
        if self.insts[i].group.is_none() {
            return Err(self.err(
                at,
                format!(
                    "`{verb}`: `{}` is not a group (declare `group max N`)",
                    self.inst_name(i)
                ),
            ));
        }
        Ok(())
    }

    fn named_pred(&self, name: &str, only: bool, at: &Pos) -> Result<i32> {
        Ok(if let Some(id) = self.kind_id(name) {
            i32::from(id) + if only { pred::ONLY } else { 0 }
        } else if self.item_named(name).is_some() {
            return Err(self.err(
                at,
                format!("`{name}` is a trait: no actor is one; match a tag instead"),
            ));
        } else if let Some(bit) = self.tags.iter().position(|t| t == name) {
            if only {
                return Err(self.err(
                    at,
                    format!("`only` applies to a kind, not the tag `{name}`"),
                ));
            }
            pred::TAG_BASE + i32::try_from(bit).expect("at most MAX_TAGS tags")
        } else if let Some(g) = self.materials.ground(name).filter(|_| !only) {
            pred::ground(g.0)
        } else if let Some(f) = self.materials.feature(name).filter(|_| !only) {
            pred::feature(f.0)
        } else {
            return Err(self.err(at, format!("unknown kind, tag or material `{name}`")));
        })
    }

    fn pred(&mut self, p: &Pred) -> Result<()> {
        let v = match p {
            Pred::Fields(terms, base, at) => {
                let n = u8::try_from(terms.len()).expect("at most 3 terms, parsed");
                let slot = self.alloc_local(at, usize::from(1 + 2 * n))?;
                match base {
                    Some(b) => self.pred(b)?,
                    None => self.push_int(pred::ANY)?,
                }
                for (f, op, value, fat) in terms {
                    let (fi, _) = self.field(f, fat)?;
                    let cmp = pred::CMP
                        .iter()
                        .position(|c| c == op)
                        .expect("a comparison");
                    let cmp = i32::try_from(cmp).expect("0..6");
                    self.push_int(i32::from(fi) << 8 | cmp)?;
                    self.expr(value)?;
                }
                self.asm.field_pred(slot, n);
                return Ok(());
            }
            Pred::Free => pred::FREE,
            Pred::Bare => pred::BARE,
            Pred::Ground(g) => pred::ground(g.0),
            Pred::Feature(f) => pred::feature(f.0),
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
                self.named_pred(name, *only, at)?
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
            Expr::Agg { at, .. } => {
                return Err(self.err(at, "an aggregate belongs in a field's law"));
            }
            Expr::Index(name, i, at) => {
                let (base, len) = self.mem_array(name, at)?;
                self.expr(i)?;
                self.asm.mem_at(base, len);
            }
            Expr::Flow { at, .. } => {
                return Err(self.err(at, "`flow` belongs in a field's law"));
            }
            Expr::Along {
                op,
                field,
                target,
                at,
            } => {
                let f = self.field(field, at)?.0;
                self.target(target)?;
                self.asm.along(f, *op);
            }
            Expr::Sees {
                target,
                field,
                eye,
                at,
            } => {
                let f = self.field(field, at)?.0;
                self.target(target)?;
                match eye {
                    Some(e) => self.expr(e)?,
                    None => self.push_int(0)?,
                }
                self.asm.sees(f);
            }
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
                        Ty::Plan => {
                            return Err(self.err(
                                at,
                                format!(
                                    "`{n}` is a plan: use `{n}.via`, `{n}.cost` or `{n}.steps`"
                                ),
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
                let slot = match (l.ty, *field) {
                    (Ty::Target, f @ 0..=1) => l.slot + f,
                    (Ty::Plan, f @ 10..=12) => l.slot + f - 10,
                    (Ty::Target, _) => {
                        return Err(self.err(at, format!("`{n}` is a target: `.dx` or `.dy`")));
                    }
                    (Ty::Plan, _) => {
                        return Err(
                            self.err(at, format!("`{n}` is a plan: `.via`, `.cost` or `.steps`"))
                        );
                    }
                    _ => return Err(self.err(at, format!("`{n}` is not a target or a plan"))),
                };
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
                let mark = self.next_local;
                self.pred(p)?;
                self.expr(r)?;
                self.asm.op(OpCode::Count);
                self.next_local = mark;
            }
            Expr::Dist(t) => {
                self.target(t)?;
                self.asm.op(OpCode::Dist);
            }
            Expr::FreeAt(t) => {
                self.target(t)?;
                self.asm.op(OpCode::FreeAt);
            }
            Expr::FitsAt(t) => {
                self.target(t)?;
                self.asm.op(OpCode::FitsAt);
            }
            Expr::IsAt(t, p) => {
                self.target(t)?;
                let mark = self.next_local;
                self.pred(p)?;
                self.asm.op(OpCode::IsAt);
                self.next_local = mark;
            }
            Expr::LookOf(t) => {
                self.target(t)?;
                self.asm.op(OpCode::LookAt);
            }
            Expr::SignalOf(t, _) => {
                self.target(t)?;
                self.asm.op(OpCode::SignalAt);
            }
            Expr::HeadsOf(t) => {
                self.target(t)?;
                self.asm.op(OpCode::HeadsAt);
            }
            Expr::Fn(op, args) => {
                for a in args {
                    self.expr(a)?;
                }
                self.asm.op(*op);
            }
            Expr::Call { name, args, at } if self.field_named(name).is_some() => {
                // `f(t)`: field `f` at a target.
                let f = self.field(name, at)?.0;
                match args.as_slice() {
                    [Arg::Target(t)] => self.target(t)?,
                    [Arg::Name(n, p)] => self.target(&Target::Named(n.clone(), p.clone()))?,
                    _ => {
                        return Err(self.err(
                            at,
                            format!("`{name}(t)` reads the field at one target (`{name}(here)`)"),
                        ));
                    }
                }
                self.asm.field_at(f);
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

    /// Is `name`, called from here, a search?
    fn callee_is_search(&self, name: &str) -> bool {
        if self.members_here.iter().any(|(n, _)| n == name) {
            return self.callee(name).is_some_and(SubAst::is_search);
        }
        self.subs.iter().any(|s| s.name == name && s.is_search())
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
        if self.in_when && self.call_acts(name, 0, Reach::Act) {
            let msg = if sub.is_search() {
                format!(
                    "`{name}` may act or `next`: a search doesn't act, `next` or call what does"
                )
            } else {
                format!("`{name}` may act or `next`: a sub called in a `when` condition must not")
            };
            return Err(self.err(at, msg));
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
                                Ty::Plan => "a plan",
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
            let gene = self
                .cur
                .is_some_and(|c| self.insts[c].genes.iter().any(|g| g.name == name));
            if gene {
                return Err(self.err(
                    at,
                    format!("cannot assign to `{name}`: a gene is inherited, never written"),
                ));
            }
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

    /// A search's code, its sub's whole body: the state into locals, the
    /// loop that takes the best open state, tests the goal and runs `next`.
    /// Returns 1 when it reached the goal (the plan held for `SearchOut`),
    /// else 0.
    fn search(&mut self, se: &SearchAst) -> Result<()> {
        let at = &se.at;
        if self.stmt_acts(&Stmt::Search(Box::new(se.clone())), 0, Reach::Act) {
            return Err(self.err(
                at,
                "a search doesn't act, `next` or call what does: it plans",
            ));
        }
        // Its goal, guess and `next` run while it searches: one at a time.
        let inner = se.next.iter().any(|s| self.stmt_acts(s, 0, Reach::Search))
            || self.cond_acts(&se.goal, 0, Reach::Search)
            || se
                .guess
                .as_ref()
                .is_some_and(|e| self.expr_acts(e, 0, Reach::Search));
        if inner {
            return Err(self.err(
                at,
                "a search starts no search in its `next`, `goal` or `guess` (nor calls what does): one search at a time",
            ));
        }
        let k = u8::try_from(se.state.len()).expect("at most SEARCH_STATE, parsed");
        // The starting values, all read before any is set.
        for e in &se.start {
            self.expr(e)?;
        }
        let base = self.alloc_local(at, usize::from(k))?;
        for (i, (name, nat)) in se.state.iter().enumerate() {
            if self.local(name).is_some() {
                return Err(self.err(nat, format!("`{name}` is already bound")));
            }
            self.hides(name, &format!("state `{name}`"), nat)?;
            self.locals.push(Local {
                name: name.clone(),
                slot: base + u8::try_from(i).expect("at most SEARCH_STATE"),
                ty: Ty::Int,
            });
        }
        for i in (0..k).rev() {
            self.asm.store(base + i);
        }
        match &se.guess {
            Some(g) => self.expr(g)?,
            None => self.push_int(0)?,
        }
        // Any expression, worked out at the call; the VM holds it to 0 to
        // SEARCH_LIMIT.
        match &se.limit {
            Some((e, _)) => self.expr(e)?,
            None => self.push_int(SEARCH_DEFAULT_LIMIT)?,
        }
        self.asm.search(OpCode::SearchBegin, base, k);
        let (top, expand, fail) = (self.asm.label(), self.asm.label(), self.asm.label());
        self.asm.bind(top);
        self.asm.search(OpCode::SearchPop, base, k).jz(fail);
        self.cond(&se.goal, expand, false)?;
        self.asm.search(OpCode::SearchDone, 0, 0).push(1).ret(true);
        self.asm.bind(expand);
        // The guess at each `try` sees the search's names, not `next`'s.
        let scope = self.locals.len();
        let outer = self.in_next.replace((base, k, se.guess.clone(), scope));
        let r = self.scoped(|g| g.stmts(&se.next));
        self.in_next = outer;
        r?;
        self.asm.jmp(top);
        self.asm.bind(fail);
        self.asm.search(OpCode::SearchFail, 0, 0).push(0).ret(true);
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
            Stmt::SetAt {
                name,
                at,
                index,
                op,
                value,
            } => {
                let (base, len) = self.mem_array(name, at)?;
                // The index once, into a local: `route[rand(8)] += 1` adds
                // to the element it read.
                self.scoped(|g| {
                    g.expr(index)?;
                    let i = g.alloc_local(at, 1)?;
                    g.asm.store(i);
                    if let Some(op) = op {
                        g.asm.load(i).mem_at(base, len);
                        g.expr(value)?;
                        g.asm.op(*op);
                    } else {
                        g.expr(value)?;
                    }
                    g.asm.load(i).set_mem_at(base, len);
                    Ok(())
                })?;
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
            Stmt::Search(se) => self.search(se)?,
            Stmt::Try {
                values,
                cost,
                via,
                at,
            } => {
                let Some((base, k, guess, scope)) = self.in_next.clone() else {
                    return Err(self.err(at, "`try` belongs in a search's `next`"));
                };
                if values.len() != usize::from(k) {
                    return Err(self.err(
                        at,
                        format!(
                            "this search's state holds {k} numbers; the `try` gives {}",
                            values.len()
                        ),
                    ));
                }
                for v in values {
                    self.expr(v)?;
                }
                match cost {
                    Some(c) => self.expr(c)?,
                    None => self.push_int(1)?,
                }
                match via {
                    Some(v) => self.expr(v)?,
                    None => self.push_int(0)?,
                }
                self.asm.search_try(k);
                // The guess reads the state by its names: the tried
                // values stand in them while it is worked out.
                match &guess {
                    Some(gu) => {
                        self.asm.search(OpCode::SearchSwap, base, k);
                        let inner = self.locals.split_off(scope.min(self.locals.len()));
                        let r = self.expr(gu);
                        self.locals.extend(inner);
                        r?;
                        self.asm.search(OpCode::SearchSwap, base, k);
                    }
                    None => self.push_int(0)?,
                }
                self.asm.search(OpCode::SearchPush, 0, 0);
            }
            Stmt::Return { value, at } => {
                if self.in_next.is_some() {
                    return Err(self.err(at, "a search's `next` doesn't `return`: it `try`s"));
                }
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
            Stmt::Split { amount, target, at } => {
                self.group_only("split", at)?;
                self.target(target)?;
                self.expr(amount)?;
                self.asm.act(Action::Split);
            }
            Stmt::Join(t, at) => {
                self.group_only("join", at)?;
                self.target(t)?;
                self.asm.act(Action::Join);
            }
            Stmt::Grow(e, at) => {
                self.group_only("grow", at)?;
                self.expr(e)?;
                self.asm.op(OpCode::Grow);
            }
            Stmt::Dig(t, _) => {
                self.target(t)?;
                self.asm.act(Action::Dig);
            }
            Stmt::Build {
                material,
                target,
                at,
            } => {
                let code = if let Some(g) = self.materials.ground(material) {
                    i32::from(g.0)
                } else if let Some(f) = self.materials.feature(material) {
                    0x100 | i32::from(f.0)
                } else {
                    return Err(self.err(
                        at,
                        format!("`build {material}`: no ground or feature `{material}`"),
                    ));
                };
                self.push_int(code)?;
                self.target(target)?;
                self.asm.act(Action::Build);
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
            Stmt::Add(field, e, at) => {
                let (f, def) = self.field(field, at)?;
                if def.policy != Policy::Free {
                    let word = if def.policy == Policy::Fixed {
                        "fixed"
                    } else {
                        "conserved"
                    };
                    return Err(self.err(
                        at,
                        format!("`add {field}`: the field is {word}, so no rule makes or destroys what it holds"),
                    ));
                }
                self.expr(e)?;
                self.asm.field_add(f);
            }
            Stmt::Harvest {
                drop,
                field,
                amount,
                at,
            } => {
                let verb = if *drop { "drop" } else { "harvest" };
                if self.cur.is_none() {
                    return Err(
                        self.err(at, format!("`{verb}` inside a sub: needs belong to a kind"))
                    );
                }
                let (f, def) = self.field(field, at)?;
                if def.policy == Policy::Fixed {
                    return Err(self.err(
                        at,
                        format!("`{verb} {field}`: the field is fixed, no actor writes it"),
                    ));
                }
                let slot = self.need_slot(field).ok_or_else(|| {
                    self.err(
                        at,
                        format!("`{verb} {field}`: this kind has no need `{field}` to carry it in"),
                    )
                })?;
                self.push_int(i32::from(f))?;
                self.push_int(i32::from(slot))?;
                self.expr(amount)?;
                self.asm
                    .act(if *drop { Action::Drop } else { Action::Harvest });
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
