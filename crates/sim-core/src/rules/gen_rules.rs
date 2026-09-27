//! Rules programs made from the grammar (`docs/GRAMMAR.md`), for property
//! tests and fuzzers. Test support only.
//!
//! A program is drawn from a list of choices: each decision takes the next
//! `u32`, and a list that has run out answers 0, which is always the
//! simplest alternative (end the list, take the leaf, leave the option
//! out). So proptest's shrinking of the list, shorter and smaller, shrinks
//! the program, and a fixed list is a fixed program.
//!
//! - [`Mode::Syntax`]: whatever the grammar derives. Names come from a small
//!   pool whatever their scope or type, and actions, `next` and `return` go
//!   anywhere: it parses, and seldom compiles.
//! - [`Mode::Valid`]: programs that compile (GRAMMAR.md §9). Every name is
//!   declared and in scope with the right type; kinds and traits merge
//!   without clashes and within the need and mem limits; declarations are in
//!   range; at most one action and one `next` on any path, none in a loop;
//!   a `when` or `if` condition calls only functions, which never act; call
//!   chains are acyclic; locals stay within 12 slots, leaving 4 for targets
//!   being evaluated. [`Program::kinds`] names the concrete kinds, for a
//!   scenario to start.
//! - [`Mode::Lively`]: valid programs, half of whose rules are `when C =>
//!   ACTION` with a simple `C` (`true`, a `chance`, a short condition), and
//!   many targets a step away, so their actors move, bite, spawn and trade:
//!   a valid program's rules seldom hold and act, and its actions seldom
//!   land (for the simulation fuzzer, `sim/fuzz.rs`).

use proptest::prelude::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Syntax,
    Valid,
    Lively,
}

#[derive(Debug, Clone)]
pub struct Program {
    pub text: String,
    /// The concrete kinds it declares (not traits), in declaration order.
    pub kinds: Vec<String>,
}

/// Programs of `mode`, each from up to 1500 choices.
pub fn program(mode: Mode) -> impl Strategy<Value = Program> {
    prop::collection::vec(any::<u32>(), 0..1500).prop_map(move |c| generate(mode, &c))
}

/// The program `choices` draw.
pub fn generate(mode: Mode, choices: &[u32]) -> Program {
    let mut g = G::new(mode, choices);
    let text = if g.strict {
        g.plan();
        g.emit()
    } else {
        g.syntax_file()
    };
    Program {
        text,
        kinds: g.kind_names,
    }
}

/// Names for the syntax mode: none reserved, some contextual.
const POOL: [&str; 16] = [
    "a", "b", "c1", "foo", "_x", "Z", "k0", "food", "water", "soil", "rock", "bare", "dx", "dy",
    "target", "pred",
];
const PRED_WORDS: [&str; 5] = ["water", "soil", "rock", "free", "bare"];
const SENSES: [&str; 15] = [
    "x", "y", "age", "light", "hour", "day", "kind", "look", "signal", "state", "hurt", "hurt_dir",
    "result", "taken", "trapped",
];
const SCENTS: [&str; 3] = ["sc0", "sc1", "sc2"];
const CMP: [&str; 6] = ["<", "<=", "==", "!=", ">=", ">"];
/// Persistent local slots in one rule or sub: 16, less 4 for `toward`,
/// `away` and `random free` while they are evaluated.
const SLOTS: u32 = 12;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Ty {
    Int,
    Target,
    Pred,
}

impl Ty {
    fn width(self) -> u32 {
        if self == Ty::Target { 2 } else { 1 }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SubKind {
    /// Returns a value, never acts: callable anywhere.
    Func,
    /// Returns nothing, may end in an action: called only where one may go.
    Proc,
}

#[derive(Debug, Clone)]
struct Sig {
    name: String,
    params: Vec<(String, Ty)>,
    kind: SubKind,
}

/// A kind or trait as its code sees it (valid mode).
#[derive(Debug, Clone, Default)]
struct Owner {
    name: String,
    is_trait: bool,
    params: Vec<String>,
    /// `extends` entries, as written.
    parents: Vec<String>,
    /// Ancestor names, for `inherit NAME`.
    ancestors: Vec<String>,
    /// Traits it reaches (indices into `owners`).
    reached: Vec<usize>,
    /// Merged: what its code may name.
    needs: Vec<String>,
    mems: Vec<String>,
    states: Vec<String>,
    /// Declared here.
    own_needs: Vec<String>,
    own_mems: Vec<String>,
    own_states: Vec<String>,
    tags: Vec<String>,
    /// Member subs: its own, and the ones its ancestors define.
    subs: Vec<Sig>,
    inherited: Vec<Sig>,
    /// Inherited states it writes a block for.
    extra_states: Vec<String>,
    /// Inherited member subs it redefines, with the same signature.
    overrides: Vec<Sig>,
}

struct G<'a> {
    c: &'a [u32],
    at: usize,
    strict: bool,
    lively: bool,
    fresh: u32,
    /// Statements left to write in the whole program.
    budget: u32,
    kind_names: Vec<String>,
    // The plan (valid mode).
    consts: Vec<String>,
    tags: Vec<String>,
    owners: Vec<Owner>,
    files: Vec<Sig>,
    /// Per kind named by a `spawn ... with`: the mems those spawns set.
    withs: Vec<(String, Vec<String>)>,
    // The scope of the code being written.
    owner: Option<usize>,
    locals: Vec<(String, Ty)>,
    slots: u32,
    calls: Vec<Sig>,
    func: bool,
    in_sub: bool,
    /// Trait parameters a constant expression may name (`extends` of a trait).
    arg_names: Vec<String>,
}

impl<'a> G<'a> {
    fn new(mode: Mode, c: &'a [u32]) -> Self {
        Self {
            c,
            at: 0,
            strict: mode != Mode::Syntax,
            lively: mode == Mode::Lively,
            fresh: 0,
            budget: 80,
            kind_names: Vec::new(),
            consts: Vec::new(),
            tags: Vec::new(),
            owners: Vec::new(),
            files: Vec::new(),
            withs: Vec::new(),
            owner: None,
            locals: Vec::new(),
            slots: 0,
            calls: Vec::new(),
            func: false,
            in_sub: false,
            arg_names: Vec::new(),
        }
    }

    // ---- choices ---------------------------------------------------------------------

    fn next(&mut self) -> u32 {
        let v = self.c.get(self.at).copied().unwrap_or(0);
        self.at += 1;
        v
    }

    /// 0 to k - 1.
    fn n(&mut self, k: usize) -> usize {
        if k <= 1 {
            0
        } else {
            (self.next() % k as u32) as usize
        }
    }

    /// True `pct` times in 100; false once the choices run out.
    fn maybe(&mut self, pct: u32) -> bool {
        self.next() % 100 + pct >= 100
    }

    fn one<T: Clone>(&mut self, v: &[T]) -> T {
        v[self.n(v.len())].clone()
    }

    fn fresh(&mut self, prefix: &str) -> String {
        self.fresh += 1;
        format!("{prefix}{}", self.fresh)
    }

    /// A pool name that is none of `not`.
    fn pool(&mut self, not: &[&str]) -> String {
        let ok: Vec<&str> = POOL.iter().copied().filter(|p| !not.contains(p)).collect();
        self.one(&ok).to_string()
    }

    /// Up to `k` distinct pool names that are none of `not`.
    fn pool_distinct(&mut self, k: usize, not: &[&str]) -> Vec<String> {
        let mut ok: Vec<&str> = POOL.iter().copied().filter(|p| !not.contains(p)).collect();
        let mut out = Vec::new();
        for _ in 0..k.min(ok.len()) {
            let i = self.n(ok.len());
            out.push(ok.remove(i).to_string());
        }
        out
    }

    /// Between statements, declarations or rules.
    fn sep(&mut self) -> &'static str {
        match self.n(8) {
            0 => "\n",
            1 => " ",
            2 => "\n  ",
            3 => "\n\n",
            4 => "  # a comment\n",
            5 => "\r\n",
            6 => " # ünïcödé, \"#\" ✓\n",
            _ => "\t",
        }
    }

    /// The optional `;` after a rule's single statement, a declaration or
    /// a `const`: now and then.
    fn semi(&mut self) -> &'static str {
        if self.maybe(20) { ";" } else { "" }
    }

    fn op(&mut self, ops: &[&str]) -> String {
        let o = self.one(ops);
        if self.maybe(20) {
            o.to_string()
        } else {
            format!(" {o} ")
        }
    }

    // ---- scope -----------------------------------------------------------------------

    fn mark(&self) -> (usize, u32) {
        (self.locals.len(), self.slots)
    }

    fn restore(&mut self, m: (usize, u32)) {
        self.locals.truncate(m.0);
        self.slots = m.1;
    }

    fn fits(&self, n: u32) -> bool {
        !self.strict || self.slots + n <= SLOTS
    }

    fn local(&mut self, name: String, ty: Ty, slots: u32) {
        self.locals.push((name, ty));
        self.slots += slots;
    }

    fn locals_of(&self, ty: Ty) -> Vec<String> {
        self.locals
            .iter()
            .filter(|(_, t)| *t == ty)
            .map(|(n, _)| n.clone())
            .collect()
    }

    fn cur(&self) -> Option<&Owner> {
        self.owner.map(|o| &self.owners[o])
    }

    /// Names that hold an int here, and whether they can be assigned.
    fn int_names(&self) -> Vec<(String, bool)> {
        let mut v: Vec<(String, bool)> = self
            .locals_of(Ty::Int)
            .into_iter()
            .map(|n| (n, true))
            .collect();
        if let Some(o) = self.cur() {
            v.extend(o.needs.iter().chain(&o.mems).map(|n| (n.clone(), true)));
            v.extend(o.params.iter().map(|n| (n.clone(), false)));
        }
        v.extend(self.consts.iter().map(|n| (n.clone(), false)));
        v
    }

    fn int_name(&mut self) -> Option<String> {
        if !self.strict {
            return Some(self.pool(&[]));
        }
        let v = self.int_names();
        (!v.is_empty()).then(|| self.one(&v).0)
    }

    fn assignable(&mut self) -> Option<String> {
        if !self.strict {
            return Some(self.pool(&[]));
        }
        let v: Vec<String> = self
            .int_names()
            .into_iter()
            .filter(|(_, w)| *w)
            .map(|(n, _)| n)
            .collect();
        (!v.is_empty()).then(|| self.one(&v))
    }

    fn named_target(&mut self) -> Option<String> {
        if !self.strict {
            return Some(self.pool(&[]));
        }
        let v = self.locals_of(Ty::Target);
        (!v.is_empty()).then(|| self.one(&v))
    }

    fn kind_name(&mut self) -> String {
        if !self.strict {
            return self.pool(&PRED_WORDS);
        }
        let v = self.kind_names.clone();
        self.one(&v)
    }

    fn states_here(&self) -> Vec<String> {
        self.cur().map(|o| o.states.clone()).unwrap_or_default()
    }

    fn needs_here(&self) -> Vec<String> {
        self.cur().map(|o| o.needs.clone()).unwrap_or_default()
    }

    fn callable(&self, kind: SubKind) -> Vec<Sig> {
        self.calls
            .iter()
            .filter(|s| s.kind == kind)
            .cloned()
            .collect()
    }

    // ---- the plan (valid mode) -------------------------------------------------------

    fn sigs(&mut self, prefix: &str, max: usize) -> Vec<Sig> {
        (0..self.n(max + 1))
            .map(|j| {
                let params = (0..self.n(4))
                    .map(|i| match self.n(4) {
                        1 => (format!("pt{i}"), Ty::Target),
                        2 => (format!("pq{i}"), Ty::Pred),
                        _ => (format!("pa{i}"), Ty::Int),
                    })
                    .collect();
                let kind = if self.maybe(50) {
                    SubKind::Func
                } else {
                    SubKind::Proc
                };
                Sig {
                    name: format!("{prefix}f{j}"),
                    params,
                    kind,
                }
            })
            .collect()
    }

    fn subset(&mut self, v: &[String], pct: u32) -> Vec<String> {
        v.iter().filter(|_| self.maybe(pct)).cloned().collect()
    }

    fn plan(&mut self) {
        let consts = self.n(4);
        self.consts = (0..consts).map(|i| format!("C{i}")).collect();
        let tag_pool: Vec<String> = (0..self.n(5)).map(|i| format!("tg{i}")).collect();
        for i in 0..self.n(3) {
            let name = format!("t{i}");
            let mut o = Owner {
                is_trait: true,
                params: (0..self.n(3)).map(|j| format!("{name}p{j}")).collect(),
                name,
                ..Owner::default()
            };
            // Earlier traits, with arguments that may use its own parameters.
            self.arg_names = o.params.clone();
            for t in 0..i {
                self.extend_trait(&mut o, t, 30);
            }
            self.arg_names.clear();
            let n = &o.name;
            for j in 0..self.n(3) {
                if o.needs.len() < 4 {
                    o.own_needs.push(format!("{n}n{j}"));
                }
            }
            for j in 0..self.n(4).min(12 - o.mems.len()) {
                o.own_mems.push(format!("{n}m{j}"));
            }
            o.own_states = (0..self.n(3)).map(|j| format!("{n}st{j}")).collect();
            o.needs.extend(o.own_needs.iter().cloned());
            o.mems.extend(o.own_mems.iter().cloned());
            o.states.extend(o.own_states.iter().cloned());
            o.tags = self.subset(&tag_pool, 30);
            o.subs = self.sigs(n, 2);
            self.owners.push(o);
        }
        let traits = self.owners.len();
        self.files = self.sigs("", 3);
        let kinds = 1 + self.n(3);
        for i in 0..kinds {
            let mut o = Owner {
                name: format!("k{i}"),
                ..Owner::default()
            };
            if i > 0 && self.maybe(40) {
                let p = traits + self.n(i);
                let p = self.owners[p].clone();
                o.parents.push(p.name.clone());
                o.ancestors = p.ancestors.clone();
                o.ancestors.push(p.name.clone());
                o.reached = p.reached.clone();
                o.needs = p.needs.clone();
                o.mems = p.mems.clone();
                o.states = p.states.clone();
                o.inherited = (p.inherited.iter().chain(&p.subs).chain(&p.overrides))
                    .cloned()
                    .collect();
            }
            for t in 0..traits {
                self.extend_trait(&mut o, t, 35);
            }
            if !o.inherited.is_empty() && self.maybe(20) {
                let sub = self.one(&o.inherited);
                o.inherited.retain(|s| s.name != sub.name);
                o.overrides.push(sub);
            }
            // The kind parent last, sometimes: parents merge in any order.
            if o.parents.len() > 1 && self.maybe(30) {
                o.parents.rotate_left(1);
            }
            // Needs: the engine's names (a redeclaration costs no slot), or its own.
            for j in 0..self.n(4) {
                let name = if self.maybe(40) {
                    self.one(&["water", "health", "food"]).to_string()
                } else {
                    format!("k{i}n{j}")
                };
                if o.own_needs.contains(&name) || (!o.needs.contains(&name) && o.needs.len() == 4) {
                    continue;
                }
                if !o.needs.contains(&name) {
                    o.needs.push(name.clone());
                }
                o.own_needs.push(name);
            }
            for j in 0..self.n(5).min(12 - o.mems.len()) {
                o.own_mems.push(format!("k{i}m{j}"));
            }
            o.mems.extend(o.own_mems.iter().cloned());
            o.extra_states = self.subset(&o.states, 30);
            o.own_states = (0..self.n(3)).map(|j| format!("k{i}st{j}")).collect();
            o.states.extend(o.own_states.iter().cloned());
            o.tags = self.subset(&tag_pool, 30);
            o.subs = self.sigs(&o.name, 2);
            self.kind_names.push(o.name.clone());
            self.owners.push(o);
        }
        for i in traits..self.owners.len() {
            let mems = self.owners[i].mems.clone();
            if !mems.is_empty() && self.maybe(40) {
                let mut w = vec![self.one(&mems)];
                let other = self.one(&mems);
                if !w.contains(&other) && self.maybe(50) {
                    w.push(other);
                }
                self.withs.push((self.owners[i].name.clone(), w));
            }
        }
        let mut tags = Vec::new();
        for t in self.owners.iter().flat_map(|o| &o.tags) {
            if !tags.contains(t) {
                tags.push(t.clone());
            }
        }
        self.tags = tags;
    }

    /// `o` extends trait `t`, sometimes, if that keeps it valid: no trait
    /// reached twice (its arguments could differ), needs and mems in range.
    fn extend_trait(&mut self, o: &mut Owner, t: usize, pct: u32) {
        let tr = self.owners[t].clone();
        let twice = o.reached.contains(&t) || tr.reached.iter().any(|r| o.reached.contains(r));
        let fits = o.needs.len() + tr.needs.len() <= 4 && o.mems.len() + tr.mems.len() <= 12;
        if twice || !fits || !self.maybe(pct) {
            return;
        }
        let args: Vec<String> = (0..tr.params.len()).map(|_| self.const_expr(2)).collect();
        o.parents.push(if args.is_empty() && self.maybe(50) {
            tr.name.clone()
        } else {
            format!("{}({})", tr.name, args.join(", "))
        });
        o.ancestors.extend(tr.ancestors.iter().cloned());
        o.ancestors.push(tr.name.clone());
        o.reached.extend(tr.reached.iter().copied());
        o.reached.push(t);
        o.needs.extend(tr.needs.iter().cloned());
        o.mems.extend(tr.mems.iter().cloned());
        o.states.extend(tr.states.iter().cloned());
        o.inherited
            .extend(tr.inherited.iter().chain(&tr.subs).cloned());
    }

    fn emit(&mut self) -> String {
        let mut out = String::new();
        for i in 0..self.consts.len() {
            let v = self.const_expr_upto(3, i);
            let semi = self.semi();
            let sep = self.sep();
            out += &format!("const {} = {v}{semi}{sep}", self.consts[i]);
        }
        let mut order: Vec<usize> = (0..self.owners.len()).collect();
        // Items may come in any order: names are global.
        let r = self.n(order.len().max(1));
        order.rotate_left(r);
        let files = self.files.clone();
        for (j, sig) in files.iter().enumerate() {
            let calls = files[..j].to_vec();
            out += &self.sub_text(sig, None, calls);
            out += "\n";
        }
        for oi in order {
            out += &self.item_text(oi);
            out += "\n";
        }
        out
    }

    fn item_text(&mut self, oi: usize) -> String {
        let o = self.owners[oi].clone();
        let mut s = if o.is_trait {
            if o.params.is_empty() && self.maybe(50) {
                format!("trait {}", o.name)
            } else {
                format!("trait {}({})", o.name, o.params.join(", "))
            }
        } else {
            format!("kind {}", o.name)
        };
        if !o.parents.is_empty() {
            s += &format!(" extends {}", o.parents.join(", "));
        }
        s += " {";
        let mut decls = self.decls(&o);
        let r = self.n(decls.len().max(1));
        decls.rotate_left(r);
        for d in decls {
            s += self.sep();
            s += &d;
            s += self.semi();
        }
        let files = self.files.clone();
        // A redefinition calls only file subs: calling what it replaces
        // through another member sub would be a cycle.
        for sig in &o.overrides {
            s += self.sep();
            s += &self.sub_text(sig, Some(oi), files.clone());
        }
        for (j, sig) in o.subs.iter().enumerate() {
            let calls: Vec<Sig> = o.subs[..j]
                .iter()
                .chain(&o.inherited)
                .chain(&files)
                .cloned()
                .collect();
            s += self.sep();
            s += &self.sub_text(sig, Some(oi), calls);
        }
        let calls: Vec<Sig> = o
            .subs
            .iter()
            .chain(&o.overrides)
            .chain(&o.inherited)
            .chain(&files)
            .cloned()
            .collect();
        s += &self.rule_list(oi, &calls, true);
        let mut states: Vec<String> = o.extra_states.clone();
        states.extend(o.own_states.iter().cloned());
        for st in states {
            s += &format!("\nstate {st} {{");
            s += &self.rule_list(oi, &calls, false);
            s += "\n}";
        }
        s + "\n}"
    }

    fn decls(&mut self, o: &Owner) -> Vec<String> {
        let mut d = Vec::new();
        if !o.is_trait {
            if self.maybe(50) {
                let c = self.one(&["a", "B", "#", "@", "*", "%", "~", "9", "."]);
                d.push(format!("glyph \"{c}\""));
            }
            if self.maybe(30) {
                d.push(format!("color \"#{:06x}\"", self.next() & 0xFF_FFFF));
            }
            if self.maybe(10) {
                d.push("cover".into());
            }
            if self.maybe(40) {
                d.push(format!("cadence {}", 1u32 << self.n(10)));
            }
            if self.maybe(40) {
                d.push(format!("sight {}", self.n(17)));
            }
            if self.maybe(20) {
                d.push(format!("fuel {}", 1 + self.n(4096)));
            }
            if self.maybe(30) {
                let v = self.time_or_int();
                d.push(format!("food {v}"));
            }
            if self.maybe(20) {
                d.push(format!("bite {}", self.n(256)));
            }
        }
        if !o.tags.is_empty() {
            d.push(format!("tags {}", o.tags.join(" ")));
        }
        for n in &o.own_needs {
            let mut s = format!("need {n} max {}", self.need_max());
            if self.maybe(40) {
                s += if self.maybe(80) {
                    " decay 0"
                } else {
                    " decay 1"
                };
            }
            if self.maybe(40) {
                s += " vital";
            }
            d.push(s);
        }
        let mut mems = o.own_mems.clone();
        while !mems.is_empty() {
            let k = 1 + self.n(mems.len());
            let line: Vec<String> = mems.drain(..k).collect();
            d.push(format!("mem {}", line.join(", ")));
        }
        d
    }

    fn need_max(&mut self) -> String {
        match self.n(4) {
            0 => format!("{}", 1 + self.n(100)),
            1 => format!("{}h", 1 + self.n(48)),
            2 => "2147483647".into(),
            _ => format!("{}", 1 + self.n(100_000)),
        }
    }

    fn time_or_int(&mut self) -> String {
        if self.maybe(50) {
            self.time()
        } else {
            format!("{}", self.n(1000))
        }
    }

    fn time(&mut self) -> String {
        match self.n(5) {
            0 => format!("{}min", self.n(120)),
            1 => format!("{}h", self.n(48)),
            2 => format!("{}d", self.n(10)),
            3 => "99420d".into(),
            _ => "143165576min".into(),
        }
    }

    /// A constant expression over the numbers and the first `upto` consts.
    fn const_expr_upto(&mut self, d: u32, upto: usize) -> String {
        if d == 0 || !self.maybe(60) {
            let names: Vec<String> = self.consts[..upto]
                .iter()
                .chain(&self.arg_names)
                .cloned()
                .collect();
            return if !names.is_empty() && self.maybe(40) {
                self.one(&names)
            } else {
                self.int_lit()
            };
        }
        match self.n(4) {
            0 => {
                let (a, b) = (
                    self.const_expr_upto(d - 1, upto),
                    self.const_expr_upto(d - 1, upto),
                );
                let op = self.op(&["+", "-", "*", "/", "%", "<", "==", ">="]);
                format!("({a}{op}{b})")
            }
            1 => format!("-{}", self.const_expr_upto(d - 1, upto)),
            2 => {
                let f = self.one(&["abs", "sign", "hi", "lo"]);
                format!("{f}({})", self.const_expr_upto(d - 1, upto))
            }
            _ => {
                let f = self.one(&["min", "max", "pack", "clamp"]);
                let mut a = vec![
                    self.const_expr_upto(d - 1, upto),
                    self.const_expr_upto(d - 1, upto),
                ];
                if f == "clamp" {
                    a.push(self.const_expr_upto(d - 1, upto));
                }
                format!("{f}({})", a.join(", "))
            }
        }
    }

    fn const_expr(&mut self, d: u32) -> String {
        let all = self.consts.len();
        self.const_expr_upto(d, all)
    }

    fn int_lit(&mut self) -> String {
        match self.n(6) {
            0 => format!("{}", self.n(10)),
            1 => self.time(),
            2 => self
                .one(&[
                    "32767",
                    "007",
                    "32768",
                    "65536",
                    "2147483647",
                    "1000000",
                    "255",
                    "256",
                ])
                .into(),
            _ => format!("{}", self.n(100)),
        }
    }

    // ---- code ------------------------------------------------------------------------

    fn enter(&mut self, owner: Option<usize>, calls: Vec<Sig>) {
        self.owner = owner;
        self.calls = calls;
        self.locals.clear();
        self.slots = 0;
    }

    fn sub_text(&mut self, sig: &Sig, owner: Option<usize>, calls: Vec<Sig>) -> String {
        // A function calls only functions: it must never act.
        let calls = if sig.kind == SubKind::Func {
            calls
                .into_iter()
                .filter(|s| s.kind == SubKind::Func)
                .collect()
        } else {
            calls
        };
        self.enter(owner, calls);
        self.func = sig.kind == SubKind::Func;
        self.in_sub = true;
        let mut params = Vec::new();
        for (p, ty) in &sig.params {
            self.local(p.clone(), *ty, ty.width());
            params.push(match ty {
                Ty::Int => p.clone(),
                Ty::Target => format!("{p}: target"),
                Ty::Pred => format!("{p}: pred"),
            });
        }
        let mut stmts = self.stmts(2, sig.kind == SubKind::Proc);
        // A function has a `return` with a value, or it is a procedure.
        if self.func {
            let e = self.expr(2);
            stmts.push((format!("return {e}"), false));
        }
        let body = self.render_block(stmts);
        self.in_sub = false;
        self.func = false;
        format!("sub {}({}) {body}", sig.name, params.join(", "))
    }

    /// Rules and `inherit`s of owner `oi`: the reflexes, or a state's.
    fn rule_list(&mut self, oi: usize, calls: &[Sig], reflex: bool) -> String {
        let o = self.owners[oi].clone();
        let mut s = String::new();
        let mut named: Vec<String> = Vec::new();
        for _ in 0..self.n(5) {
            if self.budget == 0 {
                break;
            }
            if self.maybe(15) {
                // `inherit NAME` only in the reflexes: a state's ancestor may lack it.
                let name = if reflex && !o.ancestors.is_empty() && self.maybe(50) {
                    let a = self.one(&o.ancestors);
                    (!named.contains(&a)).then_some(a)
                } else {
                    None
                };
                match name {
                    Some(a) => {
                        s += &format!("\ninherit {a}");
                        named.push(a);
                    }
                    None => s += "\ninherit",
                }
                continue;
            }
            self.enter(Some(oi), calls.to_vec());
            s += "\n";
            s += &self.rule();
        }
        s
    }

    fn rule(&mut self) -> String {
        if self.lively && self.maybe(50) {
            let c = match self.n(3) {
                0 => "true".to_string(),
                1 => format!("chance({})", 1 + self.n(99)),
                _ => self.cond(true, 1),
            };
            let a = self.action(2);
            return format!("when {c} => {a}{}", self.semi());
        }
        let c = self.cond(true, 2);
        let stmts = self.stmts(3, true);
        if stmts.len() == 1 && self.maybe(60) {
            let (st, _) = &stmts[0];
            // A bare `return` ends at the line's end: the next rule is on the next.
            format!("when {c} => {st}{}", self.semi())
        } else {
            let b = self.render_block(stmts);
            format!("when {c} => {b}")
        }
    }

    /// Statements, each with "a bare `return`: the line must end after it".
    fn render_block(&mut self, stmts: Vec<(String, bool)>) -> String {
        let mut s = String::from("{");
        for (st, bare) in stmts {
            s += self.sep();
            s += &st;
            if bare {
                s += if self.maybe(50) { ";" } else { "\n" };
            } else if self.maybe(20) {
                s += ";";
            }
        }
        s += self.sep();
        s + "}"
    }

    /// A block, in its own scope.
    fn block(&mut self, d: u32, fin: bool) -> String {
        let m = self.mark();
        let stmts = self.stmts(d, fin);
        self.restore(m);
        self.render_block(stmts)
    }

    /// Statements that neither act nor `next`, then, if `fin`, maybe one that
    /// does. In the syntax mode, anything anywhere.
    fn stmts(&mut self, d: u32, fin: bool) -> Vec<(String, bool)> {
        let mut v = Vec::new();
        for _ in 0..self.n(4) {
            if self.budget == 0 {
                break;
            }
            self.budget -= 1;
            let st = if self.strict {
                self.plain(d)
            } else {
                match self.n(3) {
                    0 => self.plain(d),
                    1 => {
                        v.extend(self.final_(d).into_iter().map(|s| (s, false)));
                        continue;
                    }
                    _ => self.ret(d),
                }
            };
            v.push(st);
        }
        if fin && self.budget > 0 && self.maybe(75) {
            self.budget -= 1;
            v.extend(self.final_(d).into_iter().map(|s| (s, false)));
        }
        v
    }

    fn ret(&mut self, d: u32) -> (String, bool) {
        if self.maybe(50) {
            (format!("return {}", self.expr(d)), false)
        } else {
            ("return".into(), true)
        }
    }

    /// A statement that neither acts nor `next`s (valid mode).
    fn plain(&mut self, d: u32) -> (String, bool) {
        let alts = if d == 0 { 6 } else { 13 };
        let st = match self.n(alts) {
            0 | 1 => match self.assignable() {
                Some(n) => format!("{n} = {}", self.expr(d)),
                None => self.let_(d),
            },
            2 => self.let_(d),
            3 => match self.assignable() {
                Some(n) => {
                    let op = self.one(&["+=", "-="]);
                    format!("{n} {op} {}", self.expr(d))
                }
                None => self.let_(d),
            },
            4 => self.effect(d),
            5 => {
                let fs = self.callable(SubKind::Func);
                if fs.is_empty() {
                    self.effect(d)
                } else {
                    let f = self.one(&fs);
                    self.call(&f, d)
                }
            }
            6 => self.if_(d, false),
            7 => {
                let m = self.mark();
                let c = self.cond(true, d - 1);
                let b = self.block(d - 1, false);
                self.restore(m);
                format!("while {c} {b}")
            }
            8 if self.fits(1) => {
                let e = self.expr(d - 1);
                let m = self.mark();
                self.slots += 1;
                let b = self.block(d - 1, false);
                self.restore(m);
                format!("repeat {e} {b}")
            }
            9 if self.fits(5) => {
                let p = self.pred();
                let r = self.additive(d - 1);
                let v = self.fresh("v");
                let m = self.mark();
                self.local(v.clone(), Ty::Target, 5);
                let b = self.block(d - 1, false);
                self.restore(m);
                format!("for each {p} within {r} as {v} {b}")
            }
            10 => self.choose(d, false),
            11 if self.in_sub && self.strict => {
                if self.func {
                    format!(
                        "if {} {{ return {} }}",
                        self.cond(false, d - 1),
                        self.expr(d - 1)
                    )
                } else {
                    return ("return".into(), true);
                }
            }
            _ => self.let_(d),
        };
        (st, false)
    }

    fn let_(&mut self, d: u32) -> String {
        let e = self.expr(d);
        if !self.fits(1) {
            return format!("look = {e}");
        }
        let name = if !self.strict {
            self.pool(&[])
        } else if !self.consts.is_empty() && self.maybe(15) {
            // A local may hide a constant.
            let c = self.one(&self.consts.clone());
            if self.locals.iter().any(|(n, _)| *n == c) {
                self.fresh("l")
            } else {
                c
            }
        } else {
            self.fresh("l")
        };
        self.local(name.clone(), Ty::Int, 1);
        format!("let {name} = {e}")
    }

    fn effect(&mut self, d: u32) -> String {
        match self.n(3) {
            0 => format!("look = {}", self.expr(d)),
            1 => format!("signal = {}", self.expr(d)),
            _ => {
                let ch = self.scent_name();
                format!("mark {ch} {}", self.expr(d))
            }
        }
    }

    fn scent_name(&mut self) -> String {
        if self.strict {
            self.one(&SCENTS).to_string()
        } else {
            self.pool(&[])
        }
    }

    fn if_(&mut self, d: u32, fin: bool) -> String {
        let d = d.max(1);
        let m = self.mark();
        let c = self.cond(true, d - 1);
        let then = self.block(d - 1, fin);
        self.restore(m);
        let mut s = format!("if {c} {then}");
        if self.maybe(50) {
            if d > 1 && self.maybe(30) {
                s += &format!(" else {}", self.if_(d - 1, fin));
            } else {
                s += &format!(" else {}", self.block(d - 1, fin));
            }
        }
        s
    }

    fn choose(&mut self, d: u32, fin: bool) -> String {
        let d = d.max(1);
        let n = 1 + self.n(3);
        if !self.fits(n as u32 + 1) {
            return self.effect(d);
        }
        let m = self.mark();
        self.slots += n as u32 + 1;
        let mut arms: Vec<(String, String, bool)> = Vec::new();
        for _ in 0..n {
            let w = self.expr(d - 1);
            let am = self.mark();
            let stmts = self.stmts(d - 1, fin);
            self.restore(am);
            let arm = if stmts.len() == 1 && self.maybe(60) {
                stmts[0].clone()
            } else {
                (self.render_block(stmts), false)
            };
            arms.push((w, arm.0, arm.1));
        }
        self.restore(m);
        let mut s = String::from("choose {");
        for i in 0..arms.len() {
            let (w, body, bare) = &arms[i];
            s += self.sep();
            s += &format!("{w}: {body}");
            // A weight that starts with `-` or `(` would continue the arm's
            // expression, and a bare `return` would take it as its value.
            let next = arms.get(i + 1).map(|a| a.0.as_str()).unwrap_or("");
            if next.starts_with('-') || next.starts_with('(') {
                s += ";";
            } else if *bare {
                s += if self.maybe(50) { ";" } else { "\n" };
            } else if self.maybe(20) {
                s += ";";
            }
        }
        s += self.sep();
        s + "}"
    }

    /// Statements that may act and `next` (once each on every path).
    fn final_(&mut self, d: u32) -> Vec<String> {
        let states = self.states_here();
        let procs = self.callable(SubKind::Proc);
        match self.n(if d == 0 { 2 } else { 5 }) {
            1 if !states.is_empty() || !self.strict => {
                let st = if self.strict {
                    self.one(&states)
                } else {
                    self.pool(&[])
                };
                let next = format!("next {st}");
                if !self.maybe(50) {
                    return vec![next];
                }
                let a = self.action(d);
                if self.maybe(50) {
                    vec![next, a]
                } else {
                    vec![a, next]
                }
            }
            2 => vec![self.if_(d, true)],
            3 => vec![self.choose(d, true)],
            4 if !procs.is_empty() || !self.strict => {
                if self.strict {
                    let p = self.one(&procs);
                    vec![self.call(&p, d)]
                } else {
                    let name = self.pool(&[]);
                    let a = self.args_any(d);
                    vec![format!("{name}({a})")]
                }
            }
            _ => vec![self.action(d)],
        }
    }

    fn action(&mut self, d: u32) -> String {
        let d = d.max(1);
        let needs = self.needs_here();
        match self.n(10) {
            0 => "idle".into(),
            1 => "die".into(),
            2 => format!("become {}", self.kind_name()),
            3 => {
                let k = self.kind_name();
                let t = self.target(d - 1);
                let mut s = format!("spawn {k} at {t}");
                let with = if self.strict {
                    self.withs
                        .iter()
                        .find(|(n, _)| *n == k)
                        .map(|(_, w)| w.clone())
                        .unwrap_or_default()
                } else {
                    let k = self.n(3);
                    self.pool_distinct(k, &[])
                };
                if !with.is_empty() && (!self.strict || self.maybe(70)) {
                    let set: Vec<String> = with
                        .iter()
                        .filter(|_| !self.strict || self.maybe(70))
                        .cloned()
                        .collect();
                    let set = if set.is_empty() {
                        with[..1].to_vec()
                    } else {
                        set
                    };
                    let vals: Vec<String> = set
                        .iter()
                        .map(|n| format!("{n} = {}", self.expr(d - 1)))
                        .collect();
                    s += &format!(" with ({})", vals.join(", "));
                }
                s
            }
            4 => format!("move {}", self.target(d - 1)),
            5 => format!("drink {}", self.target(d - 1)),
            6 => format!("eat {}", self.target(d - 1)),
            7 => format!("hit {}", self.target(d - 1)),
            8 => format!("graze {}", self.target(d - 1)),
            _ if !needs.is_empty() || !self.strict => {
                let verb = self.one(&["take", "give"]);
                let t = self.target(d - 1);
                let n = if self.strict {
                    self.one(&needs)
                } else {
                    self.pool(&[])
                };
                format!("{verb} {t} {n} {}", self.expr(d - 1))
            }
            _ => "idle".into(),
        }
    }

    fn call(&mut self, sig: &Sig, d: u32) -> String {
        let d = d.max(1);
        let args: Vec<String> = sig
            .params
            .iter()
            .map(|(_, ty)| match ty {
                Ty::Int => self.expr(d - 1),
                Ty::Target => self.target(d - 1),
                Ty::Pred => self.pred(),
            })
            .collect();
        format!("{}({})", sig.name, args.join(", "))
    }

    /// Arguments of any sort (syntax mode).
    fn args_any(&mut self, d: u32) -> String {
        let d = d.max(1);
        let args: Vec<String> = (0..self.n(4))
            .map(|_| match self.n(3) {
                0 => self.expr(d - 1),
                1 => self.target(d - 1),
                _ => self.pred(),
            })
            .collect();
        args.join(", ")
    }

    // ---- conditions and expressions --------------------------------------------------

    /// `top`: a binding here is visible in the body (a top-level conjunct).
    fn cond(&mut self, top: bool, d: u32) -> String {
        let ors = usize::from(d > 0 && self.maybe(15));
        let top = top && ors == 0;
        let mut parts = vec![self.and_cond(top, d)];
        for _ in 0..ors {
            parts.push(self.and_cond(false, d));
        }
        parts.join(" or ")
    }

    fn and_cond(&mut self, top: bool, d: u32) -> String {
        let k = 1 + if d > 0 { self.n(3) } else { 0 };
        let parts: Vec<String> = (0..k).map(|_| self.not_cond(top, d)).collect();
        parts.join(" and ")
    }

    fn not_cond(&mut self, top: bool, d: u32) -> String {
        let bind = !self.strict || (top && self.fits(2));
        match self.n(if d > 0 { 7 } else { 1 }) {
            1 => format!("not {}", self.not_cond(false, d - 1)),
            2 if bind => {
                let p = self.pred();
                let r = self.additive(d - 1);
                let v = self.fresh("v");
                self.local(v.clone(), Ty::Target, 2);
                format!("nearest {p} within {r} as {v}")
            }
            3 if bind => {
                let ch = self.scent_name();
                let r = self.additive(d - 1);
                let v = self.fresh("v");
                self.local(v.clone(), Ty::Target, 2);
                format!("sniff {ch} within {r} as {v}")
            }
            4 => format!("({})", self.cond(top, d - 1)),
            _ => self.expr(d),
        }
    }

    fn expr(&mut self, d: u32) -> String {
        let mut s = self.additive(d);
        if d > 0 && self.maybe(30) {
            let op = self.op(&CMP);
            s = format!("{s}{op}{}", self.additive(d - 1));
        }
        s
    }

    fn additive(&mut self, d: u32) -> String {
        let mut s = self.term(d);
        if d > 0 {
            for _ in 0..self.n(3) {
                let op = self.op(&["+", "-"]);
                s = format!("{s}{op}{}", self.term(d - 1));
            }
        }
        s
    }

    fn term(&mut self, d: u32) -> String {
        let mut s = self.unary(d);
        if d > 0 && self.maybe(25) {
            let op = self.op(&["*", "/", "%"]);
            s = format!("{s}{op}{}", self.unary(d - 1));
        }
        s
    }

    fn unary(&mut self, d: u32) -> String {
        if d > 0 && self.maybe(10) {
            format!("-{}", self.unary(d - 1))
        } else {
            self.primary(d)
        }
    }

    fn primary(&mut self, d: u32) -> String {
        if d == 0 {
            return self.leaf();
        }
        match self.n(13) {
            1 => format!("({})", self.expr(d - 1)),
            2 => {
                let p = self.pred();
                format!("count {p} within {}", self.unary(d - 1))
            }
            3 => {
                let f = self.one(&["rand", "chance"]);
                format!("{f}({})", self.expr(d - 1))
            }
            4 => {
                let f = self.one(&["dist", "free", "look_of", "signal_of"]);
                format!("{f}({})", self.target(d - 1))
            }
            5 => {
                let t = self.target(d - 1);
                format!("is({t}, {})", self.pred())
            }
            6 => {
                let ch = self.scent_name();
                if self.maybe(50) {
                    format!("scent({ch}, {})", self.target(d - 1))
                } else {
                    format!("scent({ch})")
                }
            }
            7 => {
                let (f, k) = self.one(&[
                    ("min", 2),
                    ("max", 2),
                    ("abs", 1),
                    ("sign", 1),
                    ("clamp", 3),
                    ("pack", 2),
                    ("hi", 1),
                    ("lo", 1),
                ]);
                let a: Vec<String> = (0..k).map(|_| self.expr(d - 1)).collect();
                format!("{f}({})", a.join(", "))
            }
            8 => match self.named_target() {
                Some(v) => format!("{v}.{}", self.one(&["dx", "dy"])),
                None => self.leaf(),
            },
            9 if !self.strict => {
                let name = self.pool(&[]);
                format!("{name}({})", self.args_any(d))
            }
            9 => {
                let fs = self.callable(SubKind::Func);
                if fs.is_empty() {
                    self.leaf()
                } else {
                    let f = self.one(&fs);
                    self.call(&f, d)
                }
            }
            _ => self.leaf(),
        }
    }

    fn leaf(&mut self) -> String {
        match self.n(8) {
            0 => format!("{}", self.n(10)),
            1 | 6 | 7 => self.int_name().unwrap_or_else(|| self.int_lit()),
            2 => self.time(),
            3 => self
                .one(&["true", "false", "blocked", "missed", "refused"])
                .into(),
            4 => self.one(&SENSES).into(),
            _ => self.int_lit(),
        }
    }

    /// A target; at `d == 0` one that holds no slot and has no expression.
    fn target(&mut self, d: u32) -> String {
        // Lively: more targets that are a step away, so actions land.
        if self.lively && self.maybe(40) {
            return match self.n(3) {
                _ if d == 0 => self.one(&["north", "east", "south", "west"]).into(),
                0 => "random free".into(),
                _ => self
                    .one(&[
                        "north",
                        "east",
                        "south",
                        "west",
                        "dir(rand(8))",
                        "at(rand(3) - 1, rand(3) - 1)",
                    ])
                    .into(),
            };
        }
        if d == 0 {
            return match self.n(4) {
                0 => "here".into(),
                1 => "attacker".into(),
                2 => self.one(&["north", "east", "south", "west"]).into(),
                _ => self.named_target().unwrap_or_else(|| "here".into()),
            };
        }
        match self.n(10) {
            0 => "here".into(),
            1 => "attacker".into(),
            2 => self.one(&["north", "east", "south", "west"]).into(),
            3 => format!("dir({})", self.expr(d - 1)),
            4 => format!("at({}, {})", self.expr(d - 1), self.expr(d - 1)),
            // `toward` holds 2 slots while it evaluates, and 4 are spare: the
            // inner target is simple, or `random free` (2 more).
            5 | 6 => {
                let w = self.one(&["toward", "away"]);
                let t = if self.maybe(20) {
                    "random free".to_string()
                } else {
                    self.target(0)
                };
                format!("{w} {t}")
            }
            7 => "random free".into(),
            _ => self.named_target().unwrap_or_else(|| "here".into()),
        }
    }

    fn pred(&mut self) -> String {
        if !self.strict {
            return match self.n(4) {
                0 => self.one(&PRED_WORDS).into(),
                1 => self.pool(&[]),
                2 => format!("only {}", self.pool(&PRED_WORDS)),
                _ => {
                    let only = if self.maybe(50) { "only " } else { "" };
                    format!("{only}{}:{}", self.pool(&PRED_WORDS), self.n(256))
                }
            };
        }
        match self.n(8) {
            0 => self.one(&PRED_WORDS).into(),
            2 => format!("only {}", self.kind_name()),
            3 => format!("{}:{}", self.kind_name(), self.n(256)),
            4 => format!("only {}:{}", self.kind_name(), self.n(256)),
            5 if !self.tags.is_empty() => {
                let t = self.tags.clone();
                self.one(&t)
            }
            6 => {
                let ps = self.locals_of(Ty::Pred);
                if ps.is_empty() {
                    self.kind_name()
                } else {
                    self.one(&ps)
                }
            }
            _ => self.kind_name(),
        }
    }

    // ---- the syntax mode ---------------------------------------------------------------

    fn syntax_file(&mut self) -> String {
        let mut out = String::new();
        for _ in 0..self.n(7) {
            match self.n(4) {
                0 => out += &self.syntax_item(false),
                1 => out += &self.syntax_item(true),
                2 => out += &self.syntax_sub(),
                _ => {
                    let (name, value) = (self.pool(&[]), self.expr(3));
                    out += &format!("const {name} = {value}{}", self.semi());
                }
            }
            out += self.sep();
        }
        out
    }

    fn syntax_sub(&mut self) -> String {
        self.enter(None, Vec::new());
        self.in_sub = true;
        let k = self.n(4);
        let params: Vec<String> = self
            .pool_distinct(k, &[])
            .into_iter()
            .map(|p| match self.n(3) {
                0 => p,
                1 => format!("{p}: target"),
                _ => format!("{p}: pred"),
            })
            .collect();
        let name = self.pool(&[]);
        let b = self.block(3, true);
        self.in_sub = false;
        format!("sub {name}({}) {b}", params.join(", "))
    }

    fn syntax_item(&mut self, is_trait: bool) -> String {
        let name = if is_trait {
            self.pool(&[])
        } else {
            self.pool(&PRED_WORDS)
        };
        let mut s = if is_trait {
            let k = self.n(4);
            let ps = self.pool_distinct(k, &[]);
            if ps.is_empty() && self.maybe(50) {
                format!("trait {name}")
            } else {
                format!("trait {name}({})", ps.join(", "))
            }
        } else {
            self.kind_names.push(name.clone());
            format!("kind {name}")
        };
        let k = self.n(4);
        let parents = self.pool_distinct(k, &[]);
        if !parents.is_empty() {
            let ps: Vec<String> = parents
                .into_iter()
                .map(|p| {
                    if self.maybe(40) {
                        let a: Vec<String> = (0..self.n(3)).map(|_| self.expr(2)).collect();
                        format!("{p}({})", a.join(", "))
                    } else {
                        p
                    }
                })
                .collect();
            s += &format!(" extends {}", ps.join(", "));
        }
        s += " {";
        // Declarations: each single one at most once; need and mem names distinct.
        let mut decls = Vec::new();
        let singles = [
            "glyph", "color", "cover", "cadence", "sight", "fuel", "food", "bite",
        ];
        for w in singles {
            if !self.maybe(30) || (is_trait && (w == "glyph" || w == "color")) {
                continue;
            }
            decls.push(match w {
                "glyph" => "glyph \"g\"".to_string(),
                "color" => "color \"#00ff7f\"".to_string(),
                "cover" => "cover".to_string(),
                _ => format!("{w} {}", self.additive(2)),
            });
        }
        if self.maybe(40) {
            let k = self.n(4);
            let tags = self.pool_distinct(k, &["food", "water", "soil", "rock", "bare"]);
            decls.push(format!("tags {}", tags.join(" ")).trim_end().to_string());
        }
        let k = self.n(5);
        let names = self.pool_distinct(k, &[]);
        let split = self.n(names.len() + 1);
        for n in &names[..split] {
            let mut d = format!("need {n} max {}", self.additive(2));
            if self.maybe(30) {
                d += &format!(" decay {}", self.n(2));
            }
            if self.maybe(30) {
                d += " vital";
            }
            decls.push(d);
        }
        if split < names.len() {
            decls.push(format!("mem {}", names[split..].join(", ")));
        }
        let r = self.n(decls.len().max(1));
        decls.rotate_left(r);
        for d in decls {
            s += self.sep();
            s += &d;
            s += self.semi();
        }
        let k = self.n(3);
        for m in self.pool_distinct(k, &[]) {
            self.enter(None, Vec::new());
            self.in_sub = true;
            let b = self.block(2, true);
            self.in_sub = false;
            s += &format!("\nsub {m}() {b}");
        }
        s += &self.syntax_rules();
        let k = self.n(3);
        for st in self.pool_distinct(k, &[]) {
            s += &format!("\nstate {st} {{{}\n}}", self.syntax_rules());
        }
        s + "\n}"
    }

    fn syntax_rules(&mut self) -> String {
        let mut s = String::new();
        for _ in 0..self.n(4) {
            if self.maybe(20) {
                s += "\ninherit";
                if self.maybe(50) {
                    s += " ";
                    s += &self.pool(&[]);
                }
                continue;
            }
            self.enter(None, Vec::new());
            s += "\n";
            s += &self.rule();
        }
        s
    }
}
