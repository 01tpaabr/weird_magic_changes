//! The author lint (`docs/PLAN-8.md` §8, `docs/RULES.md` "Debugging"):
//! structural checks over the parsed rules and the compiled kind table.
//! Each points at a likely mistake without refusing the rule set. None
//! names the built-in content; `health`, `water` and `food` are the needs
//! the engine itself reads.

use std::collections::BTreeSet;

use super::*;

/// The needs the engine reads by name.
const ENGINE_NEEDS: [&str; 3] = ["health", "water", "food"];

/// Diagnostics without repeats, in the order found.
#[derive(Default)]
struct Out(Vec<Diagnostic>);

impl Out {
    fn push(&mut self, level: Level, at: &Pos, msg: String) {
        let d = Diagnostic {
            level,
            file: at.file.clone(),
            line: at.line,
            col: at.col,
            msg,
        };
        if !self.0.contains(&d) {
            self.0.push(d);
        }
    }

    fn warn(&mut self, at: &Pos, msg: String) {
        self.push(Level::Warning, at, msg);
    }
}

/// What the rules mention anywhere, by name, and the uses the rule set as
/// a whole must answer (a scent someone marks, a signal someone sets).
#[derive(Default)]
struct Names {
    /// Every name in a value, an assignment, a call, `with`, `take`/`give`.
    all: BTreeSet<String>,
    calls: BTreeSet<String>,
    nexts: BTreeSet<String>,
    /// Names used as predicates (kinds, tags, `pred` parameters).
    preds: BTreeSet<String>,
    marks: BTreeSet<String>,
    signal_set: bool,
    /// `(name, where, who)` of each predicate naming a kind or tag.
    pred_uses: Vec<(String, Pos, String)>,
    /// `(kind, only, look, where, who)` of each `kind:look`.
    looks: Vec<(String, bool, u8, Pos, String)>,
    /// `(channel, where, who)` of each `sniff` and `scent()`.
    smells: Vec<(String, Pos, String)>,
    /// Where `signal_of` is read, and by whom.
    signal_reads: Vec<(Pos, String)>,
    /// Actions inside a `for each`.
    each_actions: Vec<Pos>,
}

/// One pass over one item's or sub's code, filling [`Names`].
struct Seen<'n> {
    names: &'n mut Names,
    who: String,
    here: Pos,
    each: u32,
}

impl Seen<'_> {
    fn stmts(&mut self, body: &[Stmt]) {
        for s in body {
            self.stmt(s);
        }
    }

    fn act(&mut self, at: &Pos) {
        if self.each > 0 {
            self.names.each_actions.push(at.clone());
        }
    }

    fn stmt(&mut self, s: &Stmt) {
        match s {
            Stmt::Set { name, value, .. } | Stmt::Assign { name, value, .. } => {
                self.names.all.insert(name.clone());
                self.expr(value);
            }
            Stmt::Let { value, .. } => self.expr(value),
            Stmt::If { cond, then, els } => {
                self.cond(cond);
                self.stmts(then);
                self.stmts(els);
            }
            Stmt::While { cond, body } => {
                self.cond(cond);
                self.stmts(body);
            }
            Stmt::Repeat { count, body } => {
                self.expr(count);
                self.stmts(body);
            }
            Stmt::Choose(arms) => {
                for (w, b) in arms {
                    self.expr(w);
                    self.stmts(b);
                }
            }
            Stmt::Call { name, args, .. } => {
                self.names.calls.insert(name.clone());
                self.names.all.insert(name.clone());
                args.iter().for_each(|a| self.arg(a));
            }
            Stmt::Return { value, .. } => value.iter().for_each(|e| self.expr(e)),
            Stmt::Idle(at) | Stmt::Die(at) => self.act(at),
            Stmt::Become { kind, at } => {
                self.names.all.insert(kind.clone());
                self.act(at);
            }
            Stmt::Spawn {
                kind,
                at,
                pos,
                with,
            } => {
                self.names.all.insert(kind.clone());
                self.target(at);
                for (n, e, _) in with {
                    self.names.all.insert(n.clone());
                    self.expr(e);
                }
                self.act(pos);
            }
            Stmt::Transfer {
                target,
                need,
                amount,
                at,
                ..
            } => {
                self.names.all.insert(need.clone());
                self.target(target);
                self.expr(amount);
                self.act(at);
            }
            Stmt::Move(t, at)
            | Stmt::Drink(t, at)
            | Stmt::Eat(t, at)
            | Stmt::Hit(t, at)
            | Stmt::Graze(t, at) => {
                self.target(t);
                self.act(at);
            }
            Stmt::Look(e) => self.expr(e),
            Stmt::Signal(e) => {
                self.names.signal_set = true;
                self.expr(e);
            }
            Stmt::Mark(ch, e, _) => {
                self.names.marks.insert(ch.clone());
                self.expr(e);
            }
            Stmt::Next(name, at) => {
                self.names.nexts.insert(name.clone());
                self.act(at);
            }
            Stmt::ForEach { pred, r, body, .. } => {
                self.pred(pred);
                self.expr(r);
                self.each += 1;
                self.stmts(body);
                self.each -= 1;
            }
        }
    }

    fn cond(&mut self, c: &Cond) {
        match c {
            Cond::Expr(e) => self.expr(e),
            Cond::Nearest { pred, r, .. } => {
                self.pred(pred);
                self.expr(r);
            }
            Cond::Sniff { ch, r, at, .. } => {
                self.names
                    .smells
                    .push((ch.clone(), at.clone(), self.who.clone()));
                self.expr(r);
            }
            Cond::And(a, b) | Cond::Or(a, b) => {
                self.cond(a);
                self.cond(b);
            }
            Cond::Not(a) => self.cond(a),
        }
    }

    fn expr(&mut self, e: &Expr) {
        match e {
            Expr::Int(_) | Expr::Sense(_) => {}
            Expr::Name(n, _) | Expr::Field(n, _, _) => {
                self.names.all.insert(n.clone());
            }
            Expr::Bin(_, a, b) => {
                self.expr(a);
                self.expr(b);
            }
            Expr::Neg(a) | Expr::Rand(a) | Expr::Chance(a) => self.expr(a),
            Expr::Count(p, r) => {
                self.pred(p);
                self.expr(r);
            }
            Expr::Dist(t) | Expr::FreeAt(t) | Expr::LookOf(t) => self.target(t),
            Expr::IsAt(t, p) => {
                self.target(t);
                self.pred(p);
            }
            Expr::SignalOf(t) => {
                self.names
                    .signal_reads
                    .push((self.here.clone(), self.who.clone()));
                self.target(t);
            }
            Expr::Scent(ch, t, at) => {
                self.names
                    .smells
                    .push((ch.clone(), at.clone(), self.who.clone()));
                t.iter().for_each(|t| self.target(t));
            }
            Expr::Fn(_, args) => args.iter().for_each(|a| self.expr(a)),
            Expr::Call { name, args, .. } => {
                self.names.calls.insert(name.clone());
                self.names.all.insert(name.clone());
                args.iter().for_each(|a| self.arg(a));
            }
        }
    }

    fn target(&mut self, t: &Target) {
        match t {
            Target::Named(n, _) => {
                self.names.all.insert(n.clone());
            }
            Target::Heading(e) => self.expr(e),
            Target::Toward(t) | Target::Away(t) => self.target(t),
            Target::At(a, b) => {
                self.expr(a);
                self.expr(b);
            }
            Target::Here | Target::Attacker | Target::Dir(..) | Target::RandomFree => {}
        }
    }

    fn arg(&mut self, a: &Arg) {
        match a {
            Arg::Expr(e) => self.expr(e),
            Arg::Target(t) => self.target(t),
            Arg::Pred(p) => self.pred(p),
            Arg::Name(n, at) => {
                self.names.all.insert(n.clone());
                self.names.preds.insert(n.clone());
                self.names
                    .pred_uses
                    .push((n.clone(), at.clone(), self.who.clone()));
            }
        }
    }

    fn pred(&mut self, p: &Pred) {
        match p {
            Pred::Kind(n, _, at) => {
                self.names.preds.insert(n.clone());
                self.names.all.insert(n.clone());
                self.names
                    .pred_uses
                    .push((n.clone(), at.clone(), self.who.clone()));
            }
            Pred::KindLook(n, look, only, at) => {
                self.names.preds.insert(n.clone());
                self.names.all.insert(n.clone());
                self.names
                    .looks
                    .push((n.clone(), *only, *look, at.clone(), self.who.clone()));
            }
            Pred::Ground(_) | Pred::Feature(_) | Pred::Free | Pred::Bare => {}
        }
    }
}

/// What a binding or a `pred` argument matches, when the lint can tell.
#[derive(Debug, Clone)]
struct Match {
    ids: Vec<u16>,
    /// As written: `meat`, `only chicken`, `flower:1`.
    shown: String,
}

/// One kind's code, followed through its rules, member subs and the file
/// subs they call (with what each call site passes).
struct Walk {
    kind: u16,
    sight: i32,
    /// Bound names (targets, `pred` parameters) and what they match.
    binds: Vec<(String, Option<Match>)>,
    /// A sub's int parameters with a constant argument.
    ints: Vec<(String, i32)>,
    depth: u32,
    eats: Option<Pos>,
    drinks: Option<Pos>,
    sets_look: bool,
    makes: BTreeSet<u16>,
}

impl Walk {
    fn bound(&self, n: &str) -> Option<&Option<Match>> {
        self.binds
            .iter()
            .rev()
            .find(|(b, _)| b == n)
            .map(|(_, m)| m)
    }
}

impl<'a> Gen<'a> {
    /// Run every check; returns the diagnostics, and fills `debug.makes`
    /// and `debug.kind_at` (for [`crate::scenario::Scenario::unseen`]).
    pub(super) fn lint(&mut self, kinds: &Kinds) -> Vec<Diagnostic> {
        let items = self.items;
        let mut out = Out::default();

        // The rule set as a whole.
        let mut names = Names::default();
        for it in items {
            for r in it
                .rules
                .iter()
                .chain(it.states.iter().flat_map(|s| s.rules.iter()))
            {
                if let RuleItem::When(rule) = r {
                    let mut seen = Seen {
                        names: &mut names,
                        who: it.name.clone(),
                        here: rule.at.clone(),
                        each: 0,
                    };
                    seen.cond(&rule.cond);
                    seen.stmts(&rule.body);
                }
            }
            for m in &it.members {
                let mut seen = Seen {
                    names: &mut names,
                    who: it.name.clone(),
                    here: m.at.clone(),
                    each: 0,
                };
                seen.stmts(&m.body);
            }
        }
        let mut sub_sets_look = false;
        for s in self.subs {
            sub_sets_look |= sets_look(&s.body);
            let mut seen = Seen {
                names: &mut names,
                who: s.name.clone(),
                here: s.at.clone(),
                each: 0,
            };
            seen.stmts(&s.body);
        }
        for c in self.consts {
            let mut seen = Seen {
                names: &mut names,
                who: c.name.clone(),
                here: c.at.clone(),
                each: 0,
            };
            seen.expr(&c.value);
        }

        // Each kind's code, followed.
        let mut sets_look_by_kind = Vec::with_capacity(self.kind_insts.len());
        self.debug.makes = Vec::with_capacity(self.kind_insts.len());
        self.debug.kind_at = Vec::with_capacity(self.kind_insts.len());
        for k in 0..self.kind_insts.len() {
            self.enter(k);
            let ki = self.kind_insts[k];
            let inst = self.insts[ki].clone();
            let it = &items[inst.item];
            let def = &kinds.defs[k];
            let file = self
                .debug
                .files
                .iter()
                .position(|f| *f == it.at.file)
                .unwrap_or(0) as u16;
            self.debug.kind_at.push((file, it.at.line, it.at.col));
            let mut w = Walk {
                kind: k as u16,
                sight: i32::from(def.sight),
                binds: Vec::new(),
                ints: Vec::new(),
                depth: 0,
                eats: None,
                drinks: None,
                sets_look: false,
                makes: BTreeSet::new(),
            };
            for list in [&inst.reflex].into_iter().chain(&inst.state_lists) {
                for rr in &list.rules {
                    self.params = self.scope_of(rr.owner);
                    self.here = rr.rule.at.clone();
                    let mark = w.binds.len();
                    self.walk_cond(kinds, &mut w, &rr.rule.cond, &mut out);
                    self.walk_stmts(kinds, &mut w, &rr.rule.body, &mut out);
                    w.binds.truncate(mark);
                }
            }
            if let Some(at) = &w.eats
                && def.need_named("food").is_none()
            {
                out.warn(
                    at,
                    format!(
                        "`{}` eats, but has no `food` need: eating gains it nothing",
                        def.name
                    ),
                );
            }
            if let Some(at) = &w.drinks
                && def.need_named("water").is_none()
            {
                out.warn(
                    at,
                    format!(
                        "`{}` drinks, but has no `water` need: drinking fills nothing",
                        def.name
                    ),
                );
            }
            let cadence = def.cadence();
            for n in &def.needs {
                if n.decays && n.vital && (n.max as u64) < cadence {
                    out.warn(
                        &it.at,
                        format!(
                            "`{}`: need `{}` (max {}) empties before its first think (cadence {})",
                            def.name,
                            n.name,
                            shown_ticks(n.max as u64),
                            shown_ticks(cadence)
                        ),
                    );
                }
            }
            sets_look_by_kind.push(w.sets_look || sub_sets_look);
            self.debug.makes.push(w.makes.into_iter().collect());
        }
        self.params.clear();

        // Uses the rule set as a whole must answer.
        for (tag, at, who) in &names.pred_uses {
            let Some(bit) = self.tags.iter().position(|t| t == tag) else {
                continue;
            };
            if self.kind_id(tag).is_none() && !kinds.tag_bits.iter().any(|b| b & (1 << bit) != 0) {
                out.warn(
                    at,
                    format!(
                        "`{who}` looks for `{tag}`, but no kind in this rule set is tagged `{tag}`"
                    ),
                );
            }
        }
        for (kind, only, look, at, who) in &names.looks {
            let Some(k) = self.kind_id(kind) else {
                continue;
            };
            let end = if *only {
                k + 1
            } else {
                kinds.family_end[usize::from(k)]
            };
            if !(k..end).any(|f| sets_look_by_kind[usize::from(f)]) {
                out.warn(
                    at,
                    format!(
                        "`{who}` looks for `{kind}:{look}`, but no rule of `{kind}` sets `look`"
                    ),
                );
            }
        }
        for (ch, at, who) in &names.smells {
            if !names.marks.contains(ch) {
                out.warn(at, format!("`{who}` smells `{ch}`, but nothing marks it"));
            }
        }
        if !names.signal_set {
            for (at, who) in &names.signal_reads {
                out.warn(
                    at,
                    format!("`{who}` reads `signal_of`, but no rule sets `signal`"),
                );
            }
        }
        for at in &names.each_actions {
            out.warn(
                at,
                "an action inside `for each` traps when the loop finds a second cell".into(),
            );
        }

        // Never used.
        let mut tags_told: BTreeSet<&str> = BTreeSet::new();
        for it in items {
            let owner = if it.is_trait { "trait" } else { "kind" };
            for (m, at) in &it.decls.mems {
                if !names.all.contains(m) {
                    out.warn(
                        at,
                        format!("mem `{m}` of {owner} `{}` is never used", it.name),
                    );
                }
            }
            for n in &it.decls.needs {
                if !ENGINE_NEEDS.contains(&n.name.as_str()) && !names.all.contains(&n.name) {
                    out.warn(
                        &n.at,
                        format!(
                            "need `{}` of {owner} `{}` is never read or set (the engine reads only `health`, `water` and `food`)",
                            n.name, it.name
                        ),
                    );
                }
            }
            for m in &it.members {
                if !names.calls.contains(&m.name) {
                    out.warn(
                        &m.at,
                        format!("sub `{}` of {owner} `{}` is never called", m.name, it.name),
                    );
                }
            }
            for (i, st) in it.states.iter().enumerate() {
                let first = i == 0
                    && self
                        .kind_insts
                        .iter()
                        .any(|&ki| self.insts[ki].states.first() == Some(&st.name));
                if !first && !names.nexts.contains(&st.name) {
                    out.warn(
                        &st.at,
                        format!(
                            "state `{}` of {owner} `{}` is never entered: no rule says `next {}`",
                            st.name, it.name, st.name
                        ),
                    );
                }
            }
            for t in &it.decls.tags {
                if !names.preds.contains(t) && tags_told.insert(t) {
                    out.push(
                        Level::Note,
                        &it.at,
                        format!("tag `{t}` is named by no predicate"),
                    );
                }
            }
        }
        for s in self.subs {
            if !names.calls.contains(&s.name) {
                out.warn(&s.at, format!("sub `{}` is never called", s.name));
            }
        }
        for c in self.consts {
            if !names.all.contains(&c.name) {
                out.warn(&c.at, format!("const `{}` is never used", c.name));
            }
        }
        out.0
    }

    /// What `p` matches here: a `pred` parameter's argument, a kind's family
    /// (or only it), the kinds carrying a tag.
    fn matcher(&self, kinds: &Kinds, w: &Walk, p: &Pred) -> Option<Match> {
        let (name, only, look) = match p {
            Pred::Kind(n, only, _) => (n, *only, None),
            Pred::KindLook(n, look, only, _) => (n, *only, Some(*look)),
            _ => return None,
        };
        if let Some(m) = w.bound(name) {
            return m.clone();
        }
        let shown = match (only, look) {
            (true, None) => format!("only {name}"),
            (true, Some(l)) => format!("only {name}:{l}"),
            (false, None) => name.clone(),
            (false, Some(l)) => format!("{name}:{l}"),
        };
        if let Some(k) = self.kind_id(name) {
            let end = if only {
                k + 1
            } else {
                kinds.family_end[usize::from(k)]
            };
            return Some(Match {
                ids: (k..end).collect(),
                shown,
            });
        }
        let bit = self.tags.iter().position(|t| t == name)?;
        Some(Match {
            ids: (0..kinds.len() as u16)
                .filter(|&k| kinds.tag_bits[usize::from(k)] & (1 << bit) != 0)
                .collect(),
            shown,
        })
    }

    /// A search radius folded where it stands: beyond the kind's sight it
    /// is clamped.
    fn radius(&self, w: &Walk, r: &Expr, at: &Pos, out: &mut Out) {
        if let Ok(v) = self.fold(r)
            && v > w.sight
        {
            let kind = &self.items[self.insts[self.kind_insts[usize::from(w.kind)]].item].name;
            out.warn(
                at,
                format!(
                    "`{kind}`: radius {v} exceeds its sight {}: clamped",
                    w.sight
                ),
            );
        }
    }

    fn walk_stmts(&mut self, kinds: &Kinds, w: &mut Walk, body: &'a [Stmt], out: &mut Out) {
        for s in body {
            self.walk_stmt(kinds, w, s, out);
        }
    }

    fn walk_stmt(&mut self, kinds: &Kinds, w: &mut Walk, s: &'a Stmt, out: &mut Out) {
        let eater = &kinds.defs[usize::from(w.kind)].name;
        match s {
            Stmt::Eat(t, at) | Stmt::Hit(t, at) | Stmt::Graze(t, at) => {
                let verb = match s {
                    Stmt::Eat(..) => "eats",
                    Stmt::Hit(..) => "hits",
                    _ => "grazes",
                };
                if !matches!(s, Stmt::Hit(..)) && w.eats.is_none() {
                    w.eats = Some(at.clone());
                }
                self.walk_target(kinds, w, t, out);
                if let Target::Named(n, _) = t
                    && let Some(Some(m)) = w.bound(n)
                {
                    for &v in &m.ids {
                        let victim = &kinds.defs[usize::from(v)];
                        if victim.need_named("health").is_none() {
                            out.warn(
                                at,
                                format!(
                                    "`{eater}` {verb} `{}`; `{}` is `{}` but has no `health` need",
                                    m.shown, victim.name, m.shown
                                ),
                            );
                        }
                    }
                }
            }
            Stmt::Drink(t, at) => {
                if w.drinks.is_none() {
                    w.drinks = Some(at.clone());
                }
                self.walk_target(kinds, w, t, out);
            }
            Stmt::Spawn { kind, at, with, .. } => {
                if let Some(k) = self.kind_id(kind) {
                    w.makes.insert(k);
                }
                self.walk_target(kinds, w, at, out);
                for (_, e, _) in with {
                    self.walk_expr(kinds, w, e, out);
                }
            }
            Stmt::Become { kind, .. } => {
                if let Some(k) = self.kind_id(kind) {
                    w.makes.insert(k);
                }
            }
            Stmt::Look(e) => {
                w.sets_look = true;
                self.walk_expr(kinds, w, e, out);
            }
            Stmt::Set { value, .. }
            | Stmt::Assign { value, .. }
            | Stmt::Let { value, .. }
            | Stmt::Signal(value)
            | Stmt::Mark(_, value, _) => self.walk_expr(kinds, w, value, out),
            Stmt::If { cond, then, els } => {
                let mark = w.binds.len();
                self.walk_cond(kinds, w, cond, out);
                self.walk_stmts(kinds, w, then, out);
                w.binds.truncate(mark);
                self.walk_stmts(kinds, w, els, out);
            }
            Stmt::While { cond, body } => {
                let mark = w.binds.len();
                self.walk_cond(kinds, w, cond, out);
                self.walk_stmts(kinds, w, body, out);
                w.binds.truncate(mark);
            }
            Stmt::Repeat { count, body } => {
                self.walk_expr(kinds, w, count, out);
                self.walk_stmts(kinds, w, body, out);
            }
            Stmt::Choose(arms) => {
                for (weight, b) in arms {
                    self.walk_expr(kinds, w, weight, out);
                    self.walk_stmts(kinds, w, b, out);
                }
            }
            Stmt::ForEach {
                pred,
                r,
                bind,
                body,
                at,
            } => {
                self.radius(w, r, at, out);
                let m = self.matcher(kinds, w, pred);
                w.binds.push((bind.clone(), m));
                self.walk_stmts(kinds, w, body, out);
                w.binds.pop();
            }
            Stmt::Call { name, args, .. } => self.walk_call(kinds, w, name, args, out),
            Stmt::Return { value, .. } => {
                if let Some(e) = value {
                    self.walk_expr(kinds, w, e, out);
                }
            }
            Stmt::Transfer { target, amount, .. } => {
                self.walk_target(kinds, w, target, out);
                self.walk_expr(kinds, w, amount, out);
            }
            Stmt::Move(t, _) => self.walk_target(kinds, w, t, out),
            Stmt::Idle(_) | Stmt::Die(_) | Stmt::Next(..) => {}
        }
    }

    fn walk_cond(&mut self, kinds: &Kinds, w: &mut Walk, c: &'a Cond, out: &mut Out) {
        match c {
            Cond::Expr(e) => self.walk_expr(kinds, w, e, out),
            Cond::Nearest { pred, r, bind, at } => {
                self.radius(w, r, at, out);
                let m = self.matcher(kinds, w, pred);
                w.binds.push((bind.clone(), m));
            }
            Cond::Sniff { r, bind, at, .. } => {
                self.radius(w, r, at, out);
                w.binds.push((bind.clone(), None));
            }
            Cond::And(a, b) | Cond::Or(a, b) => {
                self.walk_cond(kinds, w, a, out);
                self.walk_cond(kinds, w, b, out);
            }
            Cond::Not(a) => self.walk_cond(kinds, w, a, out),
        }
    }

    fn walk_expr(&mut self, kinds: &Kinds, w: &mut Walk, e: &'a Expr, out: &mut Out) {
        match e {
            Expr::Count(p, r) => {
                let at = match p {
                    Pred::Kind(_, _, at) | Pred::KindLook(_, _, _, at) => at.clone(),
                    _ => self.here.clone(),
                };
                self.radius(w, r, &at, out);
                self.walk_expr(kinds, w, r, out);
            }
            Expr::Bin(_, a, b) => {
                self.walk_expr(kinds, w, a, out);
                self.walk_expr(kinds, w, b, out);
            }
            Expr::Neg(a) | Expr::Rand(a) | Expr::Chance(a) => self.walk_expr(kinds, w, a, out),
            Expr::Fn(_, args) => {
                for a in args {
                    self.walk_expr(kinds, w, a, out);
                }
            }
            Expr::Call { name, args, .. } => self.walk_call(kinds, w, name, args, out),
            Expr::Dist(t)
            | Expr::FreeAt(t)
            | Expr::IsAt(t, _)
            | Expr::LookOf(t)
            | Expr::SignalOf(t) => self.walk_target(kinds, w, t, out),
            Expr::Scent(_, t, _) => {
                if let Some(t) = t {
                    self.walk_target(kinds, w, t, out);
                }
            }
            Expr::Int(_) | Expr::Name(..) | Expr::Field(..) | Expr::Sense(_) => {}
        }
    }

    fn walk_target(&mut self, kinds: &Kinds, w: &mut Walk, t: &'a Target, out: &mut Out) {
        match t {
            Target::Heading(e) => self.walk_expr(kinds, w, e, out),
            Target::Toward(t) | Target::Away(t) => self.walk_target(kinds, w, t, out),
            Target::At(a, b) => {
                self.walk_expr(kinds, w, a, out);
                self.walk_expr(kinds, w, b, out);
            }
            Target::Named(..)
            | Target::Here
            | Target::Attacker
            | Target::Dir(..)
            | Target::RandomFree => {}
        }
    }

    /// Follow a call into a member sub of the kind, else a file sub, with
    /// what each argument is here: a `pred`'s match, a target's binding, an
    /// int's constant.
    fn walk_call(
        &mut self,
        kinds: &Kinds,
        w: &mut Walk,
        name: &str,
        args: &'a [Arg],
        out: &mut Out,
    ) {
        if w.depth >= 8 {
            return;
        }
        let ki = self.kind_insts[usize::from(w.kind)];
        let (sub, owner): (&'a SubAst, Option<usize>) =
            match self.insts[ki].members.iter().find(|(n, ..)| n == name) {
                Some(&(_, owner, sub)) => (sub, Some(owner)),
                None => match self.subs.iter().find(|s| s.name == name) {
                    Some(s) => (s, None),
                    None => return,
                },
            };
        let mut binds = Vec::new();
        let mut ints = Vec::new();
        for ((p, ty), a) in sub.params.iter().zip(args) {
            match (ty, a) {
                (Ty::Pred, Arg::Pred(pred)) => {
                    binds.push((p.clone(), self.matcher(kinds, w, pred)))
                }
                (Ty::Pred, Arg::Name(n, at)) => {
                    let pred = Pred::Kind(n.clone(), false, at.clone());
                    binds.push((p.clone(), self.matcher(kinds, w, &pred)));
                }
                (Ty::Target, Arg::Name(n, _) | Arg::Target(Target::Named(n, _))) => {
                    binds.push((p.clone(), w.bound(n).cloned().flatten()));
                }
                (Ty::Int, Arg::Expr(e)) => {
                    if let Ok(v) = self.fold(e) {
                        ints.push((p.clone(), v));
                    }
                }
                (Ty::Int, Arg::Name(n, at)) => {
                    if let Ok(v) = self.fold(&Expr::Name(n.clone(), at.clone())) {
                        ints.push((p.clone(), v));
                    }
                }
                _ => {}
            }
        }
        let saved = (
            std::mem::replace(&mut w.binds, binds),
            std::mem::take(&mut w.ints),
            self.params.clone(),
        );
        // A member sub sees its owner's parameters; a file sub only its own.
        self.params = owner.map(|o| self.scope_of(o)).unwrap_or_default();
        self.params.extend(ints.iter().cloned());
        w.ints = ints;
        w.depth += 1;
        self.walk_stmts(kinds, w, &sub.body, out);
        w.depth -= 1;
        (w.binds, w.ints, self.params) = saved;
    }
}

/// Does this body (or anything nested in it) set `look`?
fn sets_look(body: &[Stmt]) -> bool {
    body.iter().any(|s| match s {
        Stmt::Look(_) => true,
        Stmt::If { then, els, .. } => sets_look(then) || sets_look(els),
        Stmt::While { body, .. } | Stmt::Repeat { body, .. } | Stmt::ForEach { body, .. } => {
            sets_look(body)
        }
        Stmt::Choose(arms) => arms.iter().any(|(_, b)| sets_look(b)),
        _ => false,
    })
}

/// Ticks as the rules write them: `30min`, `2h`, `1d`, else a count.
fn shown_ticks(t: u64) -> String {
    let (d, h, m) = (days(1), hours(1), minutes(1));
    if t > 0 && t.is_multiple_of(d) {
        format!("{}d", t / d)
    } else if t > 0 && t.is_multiple_of(h) {
        format!("{}h", t / h)
    } else if t > 0 && t.is_multiple_of(m) {
        format!("{}min", t / m)
    } else {
        format!("{t} ticks")
    }
}

#[cfg(test)]
mod tests {
    use super::super::compile;

    /// The warnings and notes a rule set gets, as `line: message`.
    fn lint(text: &str) -> Vec<String> {
        let k = compile("t.rules", text).unwrap_or_else(|e| panic!("{e}"));
        k.debug
            .diagnostics
            .iter()
            .map(|d| format!("{}: {}", d.line, d.msg))
            .collect()
    }

    fn has(text: &str, want: &str) -> bool {
        lint(text).iter().any(|d| d.contains(want))
    }

    #[test]
    fn a_tag_no_kind_carries() {
        let bad = "trait prey { tags meat }
                   kind wolf { need food max 1d vital  when nearest meat within 3 as m => eat m }";
        assert!(has(
            bad,
            "`wolf` looks for `meat`, but no kind in this rule set is tagged `meat`"
        ));
        let good = "kind hen { tags meat  need health max 5 decay 0 vital }
                    kind wolf { need food max 1d vital  when nearest meat within 3 as m => eat m }";
        assert!(!has(good, "looks for `meat`"));
    }

    #[test]
    fn scents_signals_and_looks_nobody_sets() {
        let bad = "kind ant { mem a, b
                     when sniff musk within 3 as v => move toward v
                     when a == 0 and scent(musk) > 0 => a = signal_of(here)
                     when nearest ant:2 within 3 as o => b = 1 }";
        let got = lint(bad);
        assert!(
            got.iter()
                .any(|d| d.contains("`ant` smells `musk`, but nothing marks it")),
            "{got:?}"
        );
        assert!(
            got.iter()
                .any(|d| d.contains("`ant` reads `signal_of`, but no rule sets `signal`")),
            "{got:?}"
        );
        assert!(
            got.iter()
                .any(|d| d.contains("`ant` looks for `ant:2`, but no rule of `ant` sets `look`")),
            "{got:?}"
        );
        let good = "kind ant { mem a, b
                      when true => { mark musk 9  signal = 3  look = 2 }
                      when sniff musk within 3 as v => move toward v
                      when a == 0 and scent(musk) > 0 => a = signal_of(here)
                      when nearest ant:2 within 3 as o => b = 1 }";
        let got = lint(good);
        assert!(
            !got.iter()
                .any(|d| d.contains("nothing marks") || d.contains("no rule sets")),
            "{got:?}"
        );
    }

    #[test]
    fn eaten_kinds_need_health_and_eaters_food() {
        let bad = "kind rock_pile { tags stuff }
                   sub peck(what: pred) { if nearest what within 1 as s { eat s } }
                   kind hen { when true => peck(stuff) }
                   kind cow { need food max 1d vital  when nearest rock_pile within 1 as r => hit r }
                   kind duck { when true => drink random free }";
        let got = lint(bad);
        assert!(
            got.iter().any(|d| d
                .contains("`hen` eats `stuff`; `rock_pile` is `stuff` but has no `health` need")),
            "{got:?}"
        );
        assert!(
            got.iter()
                .any(|d| d.contains("`hen` eats, but has no `food` need")),
            "{got:?}"
        );
        assert!(
            got.iter().any(|d| d.contains(
                "`cow` hits `rock_pile`; `rock_pile` is `rock_pile` but has no `health` need"
            )),
            "{got:?}"
        );
        assert!(
            got.iter()
                .any(|d| d.contains("`duck` drinks, but has no `water` need")),
            "{got:?}"
        );
        let good = "kind seed { tags stuff  need health max 1 decay 0 vital }
                    sub peck(what: pred) { if nearest what within 1 as s { eat s } }
                    kind hen { need food max 1d vital  when true => peck(stuff) }
                    kind duck { need water max 4h vital  when true => drink random free }";
        let got = lint(good);
        assert!(
            !got.iter()
                .any(|d| d.contains("health") || d.contains("food") || d.contains("water")),
            "{got:?}"
        );
    }

    #[test]
    fn a_radius_beyond_sight() {
        let bad = "trait looker(r) { when count water within r > 0 => idle }
                   sub scan(n) { if nearest free within n as c { move toward c } }
                   kind owl extends looker(10) { sight 8
                     inherit looker
                     when true => scan(12) }";
        let got = lint(bad);
        assert!(
            got.iter()
                .any(|d| d.contains("`owl`: radius 10 exceeds its sight 8: clamped")),
            "{got:?}"
        );
        assert!(
            got.iter()
                .any(|d| d.contains("radius 12 exceeds its sight 8")),
            "{got:?}"
        );
        let good = "trait looker(r) { when count water within r > 0 => idle }
                    kind owl extends looker(8) { sight 8  inherit looker }";
        assert!(!has(good, "exceeds its sight"));
    }

    #[test]
    fn a_need_that_empties_before_the_first_think() {
        assert!(has(
            "kind moth { cadence 1024  need water max 30min vital }",
            "`moth`: need `water` (max 30min) empties before its first think (cadence 1024 ticks)"
        ));
        assert!(!has(
            "kind moth { cadence 64  need water max 30min vital }",
            "empties"
        ));
        assert!(
            !has(
                "kind moth { cadence 1024  need water max 30min }",
                "empties"
            ),
            "not vital"
        );
    }

    #[test]
    fn an_action_inside_for_each() {
        assert!(has(
            "kind ant { mem n  when true => for each free within 1 as c { move toward c } }",
            "an action inside `for each` traps when the loop finds a second cell"
        ));
        assert!(!has(
            "kind ant { mem n  when true => { for each free within 1 as c { n += 1 }  idle } }",
            "inside `for each`"
        ));
    }

    #[test]
    fn what_is_never_used() {
        let bad = "const K = 3
                   sub spare() { idle }
                   trait t { mem lost  sub unused() { idle } }
                   kind ant extends t { need hope max 1d  mem lost2  tags thing
                     inherit t
                     when true => idle
                     state A { when true => idle }
                     state B { when true => idle } }";
        let got = lint(bad);
        for want in [
            "const `K` is never used",
            "sub `spare` is never called",
            "sub `unused` of trait `t` is never called",
            "mem `lost` of trait `t` is never used",
            "mem `lost2` of kind `ant` is never used",
            "need `hope` of kind `ant` is never read or set",
            "state `B` of kind `ant` is never entered: no rule says `next B`",
            "tag `thing` is named by no predicate",
        ] {
            assert!(got.iter().any(|d| d.contains(want)), "{want}: {got:?}");
        }
        assert!(
            !got.iter().any(|d| d.contains("state `A`")),
            "the first state is where it starts"
        );
        let good = "const K = 3
                    sub spare() { idle }
                    kind ant { need hope max 1d  need health max 3 decay 0 vital  mem m  tags thing
                      when hope < K and nearest thing within 1 as o => { m += 1  next B }
                      when hope > 20h => spare()
                      state A { when true => idle }
                      state B { when true => idle } }";
        let got = lint(good);
        assert!(got.is_empty(), "{got:?}");
    }
}
