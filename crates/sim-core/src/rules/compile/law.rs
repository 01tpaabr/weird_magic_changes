//! A field's law (`every N { ... }`) compiled to a column program
//! ([`crate::rules::law`]). Every value is a column, one per op: a name
//! is the column it was last given, `if`/`else` runs both branches and
//! selects per cell. Inside an aggregate a field's name is its value before
//! the law ran: the neighbours as they stood, never what the law made of
//! them (those are not known across a chunk's border). What can't run over whole columns (actions, searches,
//! subs, loops, an actor's senses) is an error that says so: that belongs to
//! an actor.

use super::*;
use crate::rules::law::{Agg, Bin, Law, LawOp, MAX_LAW_OPS, Un};

struct L<'g, 'a> {
    g: &'g Gen<'a>,
    /// The law's own field.
    field: usize,
    at: Pos,
    ops: Vec<LawOp>,
    /// Columns that hold an aggregate, through any op.
    agg: Vec<bool>,
    /// Field columns already loaded.
    loaded: Vec<(u8, u16)>,
    /// Names and their columns: the field's own value first, then locals,
    /// innermost last.
    env: Vec<(String, u16)>,
    draws: u16,
    /// Inside an aggregate: a field's name is its value before the law ran,
    /// as the neighbours stood.
    in_agg: bool,
}

impl Gen<'_> {
    /// Compile field `field`'s law.
    pub(super) fn law(&self, field: usize, every: &Expr, body: &[Stmt], at: &Pos) -> Result<Law> {
        let n = self.fold(every, at)?;
        let every = u32::try_from(n)
            .ok()
            .filter(|&n| n > 0)
            .ok_or_else(|| self.err(at, format!("`every {n}`: a law runs every 1 tick or more")))?;
        let mut l = L {
            g: self,
            field,
            at: at.clone(),
            ops: Vec::new(),
            agg: Vec::new(),
            loaded: Vec::new(),
            env: Vec::new(),
            draws: 0,
            in_agg: false,
        };
        let own = l.field_col(field)?;
        l.env.push((self.field_defs[field].name.clone(), own));
        l.stmts(body)?;
        let mut law = Law {
            every,
            result: l.env[0].1,
            ops: l.ops,
            quiet: false,
            in_step: false,
        };
        law.quiet = law.leaves_zero();
        law.in_step = law.ops.iter().any(|o| matches!(o, LawOp::Flow(..)));
        Ok(law)
    }
}

impl L<'_, '_> {
    fn err(&self, at: &Pos, msg: impl Into<String>) -> CompileError {
        self.g.err(at, msg)
    }

    fn op(&mut self, op: LawOp, agg: bool) -> Result<u16> {
        if self.ops.len() == MAX_LAW_OPS {
            return Err(self.err(
                &self.at,
                format!("a law of more than {MAX_LAW_OPS} steps: split it, or make it simpler"),
            ));
        }
        self.ops.push(op);
        self.agg.push(agg);
        Ok(u16::try_from(self.ops.len() - 1).expect("at most MAX_LAW_OPS ops"))
    }

    fn any_agg(&self, cols: &[u16]) -> bool {
        cols.iter().any(|&c| self.agg[usize::from(c)])
    }

    fn field_col(&mut self, f: usize) -> Result<u16> {
        let f = u8::try_from(f).expect("at most MAX_FIELDS fields");
        if let Some(&(_, c)) = self.loaded.iter().find(|(g, _)| *g == f) {
            return Ok(c);
        }
        let c = self.op(LawOp::Field(f), false)?;
        self.loaded.push((f, c));
        Ok(c)
    }

    fn bin(&mut self, b: Bin, x: u16, y: u16) -> Result<u16> {
        let agg = self.any_agg(&[x, y]);
        self.op(LawOp::Bin(b, x, y), agg)
    }

    fn stmts(&mut self, body: &[Stmt]) -> Result<()> {
        body.iter().try_for_each(|s| self.stmt(s))
    }

    fn stmt(&mut self, s: &Stmt) -> Result<()> {
        match s {
            Stmt::Let { name, at, value } => {
                if self.g.field_named(name).is_some() {
                    return Err(self.err(
                        at,
                        format!("`{name}` is a field: a local takes another name"),
                    ));
                }
                if self.env.iter().any(|(n, _)| n == name) {
                    return Err(self.err(at, format!("`{name}` is already a local here")));
                }
                let v = self.expr(value)?;
                self.env.push((name.clone(), v));
            }
            Stmt::Set { name, at, value } => {
                let v = self.expr(value)?;
                self.assign(name, at, v)?;
            }
            Stmt::Assign {
                name,
                at,
                op,
                value,
            } => {
                let old = self.name(name, at)?;
                let v = self.expr(value)?;
                let b = if *op == OpCode::Sub {
                    Bin::Sub
                } else {
                    Bin::Add
                };
                let v = self.bin(b, old, v)?;
                self.assign(name, at, v)?;
            }
            Stmt::If { cond, then, els } => {
                let m = self.cond(cond)?;
                let base = self.env.clone();
                self.stmts(then)?;
                let after_then: Vec<u16> = self.env[..base.len()].iter().map(|e| e.1).collect();
                self.env = base.clone();
                self.stmts(els)?;
                self.env.truncate(base.len());
                for (j, t) in after_then.into_iter().enumerate() {
                    let e = self.env[j].1;
                    if t != e {
                        let agg = self.any_agg(&[m, t, e]);
                        self.env[j].1 = self.op(LawOp::Select(m, t, e), agg)?;
                    }
                }
            }
            _ => {
                return Err(self.err(
                    &self.at,
                    "a law holds `let`, assignments and `if` only: what acts, loops or calls is an actor's",
                ));
            }
        }
        Ok(())
    }

    /// Give `name` (the law's field or a local) the column `v`.
    fn assign(&mut self, name: &str, at: &Pos, v: u16) -> Result<()> {
        match self.env.iter().rposition(|(n, _)| n == name) {
            Some(j) => {
                self.env[j].1 = v;
                Ok(())
            }
            None if self.g.field_named(name).is_some() => Err(self.err(
                at,
                format!(
                    "a law writes only its own field `{}`, not `{name}`",
                    self.g.field_defs[self.field].name
                ),
            )),
            None => Err(self.err(at, format!("unknown name `{name}`: declare it with `let`"))),
        }
    }

    /// A name's column: a local or the field's value so far, another field,
    /// a constant.
    fn name(&mut self, name: &str, at: &Pos) -> Result<u16> {
        if self.in_agg
            && let Some((f, _)) = self.g.field_named(name)
        {
            return self.field_col(usize::from(f));
        }
        if let Some(&(_, c)) = self.env.iter().rev().find(|(n, _)| n == name) {
            return Ok(c);
        }
        if let Some((f, _)) = self.g.field_named(name) {
            return self.field_col(usize::from(f));
        }
        if let Some(v) = self.g.const_value(name) {
            return self.op(LawOp::Const(v), false);
        }
        Err(self.err(
            at,
            format!("unknown name `{name}`: a law reads fields, its locals and constants"),
        ))
    }

    fn expr(&mut self, e: &Expr) -> Result<u16> {
        Ok(match e {
            Expr::Int(v) => self.op(LawOp::Const(*v), false)?,
            Expr::Name(n, at) => self.name(n, at)?,
            Expr::Sense(s) => {
                let k = match s {
                    Sense::Hour => 0,
                    Sense::Day => 1,
                    Sense::Light => 2,
                    _ => {
                        return Err(self.err(
                            &self.at,
                            "a law reads only the clock of the senses: `hour`, `day`, `light`",
                        ));
                    }
                };
                self.op(LawOp::Clock(k), false)?
            }
            Expr::Bin(op, a, b) => {
                let b2 = match op {
                    OpCode::Add => Bin::Add,
                    OpCode::Sub => Bin::Sub,
                    OpCode::Mul => Bin::Mul,
                    OpCode::Div => Bin::Div,
                    OpCode::Mod => Bin::Mod,
                    OpCode::Lt => Bin::Lt,
                    OpCode::Le => Bin::Le,
                    OpCode::Eq => Bin::Eq,
                    OpCode::Ne => Bin::Ne,
                    OpCode::Ge => Bin::Ge,
                    OpCode::Gt => Bin::Gt,
                    _ => return Err(self.err(&self.at, "an operator a law doesn't know")),
                };
                let (x, y) = (self.expr(a)?, self.expr(b)?);
                self.bin(b2, x, y)?
            }
            Expr::Neg(a) => {
                let x = self.expr(a)?;
                let agg = self.any_agg(&[x]);
                self.op(LawOp::Un(Un::Neg, x), agg)?
            }
            Expr::Rand(a) | Expr::Chance(a) => {
                let x = self.expr(a)?;
                let agg = self.any_agg(&[x]);
                let draw = self.draws;
                self.draws += 1;
                let op = if matches!(e, Expr::Rand(_)) {
                    LawOp::Rand(x, draw)
                } else {
                    LawOp::Chance(x, draw)
                };
                self.op(op, agg)?
            }
            Expr::Fn(op, args) => {
                let cols = args
                    .iter()
                    .map(|a| self.expr(a))
                    .collect::<Result<Vec<_>>>()?;
                let agg = self.any_agg(&cols);
                match (op, cols.as_slice()) {
                    (OpCode::Min, &[x, y]) => self.op(LawOp::Bin(Bin::Min, x, y), agg)?,
                    (OpCode::Max, &[x, y]) => self.op(LawOp::Bin(Bin::Max, x, y), agg)?,
                    (OpCode::Abs, &[x]) => self.op(LawOp::Un(Un::Abs, x), agg)?,
                    (OpCode::Sign, &[x]) => self.op(LawOp::Un(Un::Sign, x), agg)?,
                    (OpCode::Clamp, &[x, l, h]) => self.op(LawOp::Clamp(x, l, h), agg)?,
                    _ => {
                        return Err(self.err(
                            &self.at,
                            "a law has `min`, `max`, `abs`, `sign`, `clamp`, `rand` and `chance` of the functions",
                        ));
                    }
                }
            }
            Expr::Agg { op, of, r, at } => {
                let radius = self.g.fold(r, at)?;
                if radius != 1 {
                    return Err(self.err(
                        at,
                        format!("a law sees its neighbours within 1, not {radius}"),
                    ));
                }
                self.in_agg = true;
                let src = match &**of {
                    AggOf::Field(n, p) => self.name(n, p),
                    AggOf::Cond(c) => self.cond(c),
                };
                self.in_agg = false;
                let src = src?;
                if self.agg[usize::from(src)] {
                    return Err(self.err(
                        at,
                        "an aggregate inside an aggregate: compute it in two laws",
                    ));
                }
                match op {
                    AggOp::Sum | AggOp::Count => self.op(LawOp::Agg(Agg::Sum, src), true)?,
                    AggOp::Min => self.op(LawOp::Agg(Agg::Min, src), true)?,
                    AggOp::Max => self.op(LawOp::Agg(Agg::Max, src), true)?,
                    AggOp::Avg => {
                        let sum = self.op(LawOp::Agg(Agg::Sum, src), true)?;
                        let nine = self.op(LawOp::Const(9), false)?;
                        self.bin(Bin::Div, sum, nine)?
                    }
                }
            }
            Expr::IsAt(t, p) => {
                if !matches!(t, Target::Here) {
                    return Err(self.err(
                        &self.at,
                        "a law reads actors on its own cell: `is(here, ...)`, and its neighbours through `count`",
                    ));
                }
                let v = match p {
                    Pred::Kind(name, only, at) => self.g.named_pred(name, *only, at)?,
                    Pred::KindLook(name, look, only, at) => match self.g.kind_id(name) {
                        Some(id) => pred::kind_look(id, *look) + if *only { pred::ONLY } else { 0 },
                        None => {
                            return Err(
                                self.err(at, format!("`{name}:{look}`: `{name}` is not a kind"))
                            );
                        }
                    },
                    Pred::Ground(g) => pred::ground(g.0),
                    Pred::Feature(f) => pred::feature(f.0),
                    Pred::Free => pred::FREE,
                    Pred::Bare => pred::BARE,
                    Pred::Fields(_, _, at) => {
                        return Err(self.err(
                            at,
                            "in a law a field condition is a plain condition: `is(here, ...)` takes a kind, a tag or a material",
                        ));
                    }
                };
                self.op(LawOp::Is(v), false)?
            }
            Expr::Flow {
                of,
                level,
                rate,
                at,
            } => {
                let k = self.g.fold(rate, at)?;
                if k <= 0 {
                    return Err(self.err(at, format!("`flow`'s rate is a number above 0, not {k}")));
                }
                // As in an aggregate: the neighbours as they stood.
                self.in_agg = true;
                let cols = self.expr(of).and_then(|w| Ok((w, self.expr(level)?)));
                self.in_agg = false;
                let (w, l) = cols?;
                if self.any_agg(&[w, l]) {
                    return Err(self.err(
                        at,
                        "an aggregate or a flow inside a flow: compute it in two laws",
                    ));
                }
                self.op(LawOp::Flow(w, l, k), true)?
            }
            _ => {
                return Err(self.err(
                    &self.at,
                    "a law reads fields, constants and the clock: searches, targets and calls are an actor's",
                ));
            }
        })
    }

    /// A condition as a column of 0s and 1s.
    fn cond(&mut self, c: &Cond) -> Result<u16> {
        match c {
            Cond::Expr(e) => {
                let x = self.expr(e)?;
                let zero = self.op(LawOp::Const(0), false)?;
                self.bin(Bin::Ne, x, zero)
            }
            Cond::And(a, b) => {
                let (x, y) = (self.cond(a)?, self.cond(b)?);
                self.bin(Bin::And, x, y)
            }
            Cond::Or(a, b) => {
                let (x, y) = (self.cond(a)?, self.cond(b)?);
                self.bin(Bin::Or, x, y)
            }
            Cond::Not(a) => {
                let x = self.cond(a)?;
                let agg = self.any_agg(&[x]);
                self.op(LawOp::Un(Un::Not, x), agg)
            }
            Cond::Nearest { at, .. } | Cond::Best { at, .. } | Cond::Search { at, .. } => Err(self
                .err(
                    at,
                    "a law doesn't search: `nearest`, `best` and searches are an actor's",
                )),
        }
    }
}
