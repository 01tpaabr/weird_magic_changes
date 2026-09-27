//! The scenario test's `expect` lines, evaluated: what [`expect`] finds in
//! a world, and what [`check_expect`] finds the rules lack without one.
//! The parser is [`super::Scenario::parse`]; `wmc scenario` and `wmc lint
//! --scenario` call these (as `sim::expect` and `sim::check_expect`).

use bevy_ecs::prelude::*;

use super::{Agg, Expect, Who};
use crate::actors::{ChunkActors, ChunkMinds};
use crate::rules::Kinds;
use crate::sim::{Tick, checksum};
use crate::stage::{self, Pos};

/// The kind ids an `expect` looks at: a kind's family, `only` the kind
/// alone, as in the rules. An error when the rules have no such kind.
pub fn expect_family(kinds: &Kinds, who: &Who) -> Result<std::ops::Range<u16>, String> {
    let k = kinds
        .by_name(&who.kind)
        .ok_or_else(|| format!("the rules have no kind `{}`", who.kind))?
        .id;
    let end = if who.only {
        k + 1
    } else {
        kinds.family_end[usize::from(k)]
    };
    Ok(k..end)
}

/// Kind `k`'s slot for `name`, per kind (a family's kinds may lay slots out
/// differently): a need slot of that name, else a mem slot. `true`: a need.
fn expect_slot(kinds: &Kinds, k: u16, name: &str) -> Option<(bool, usize)> {
    let d = kinds.def(k);
    d.need_named(name)
        .map(|i| (true, i))
        .or_else(|| d.mems.iter().position(|m| m == name).map(|i| (false, i)))
}

/// Whether the rules define every kind, need and memory `e` names, without
/// a world: `wmc lint --scenario` checks a test's `expect` lines with it.
/// The same errors `expect` gives.
pub fn check_expect(kinds: &Kinds, e: &Expect) -> Result<(), String> {
    match e {
        Expect::Count { who, .. } | Expect::Tally { who, .. } => {
            expect_family(kinds, who)?;
        }
        Expect::Value { name, who, .. } => {
            if !expect_family(kinds, who)?.any(|k| expect_slot(kinds, k, name).is_some()) {
                return Err(format!("`{}` has no need or memory `{name}`", who.kind));
            }
        }
        Expect::At { who: Some(w), .. } => {
            expect_family(kinds, w)?;
        }
        Expect::At { who: None, .. } | Expect::Checksum(_) | Expect::State(_) => {}
    }
    Ok(())
}

/// What a scenario test's `expect` finds in `world` (`wmc scenario`):
/// whether it holds, and what it saw, for the report. An error when it
/// names a kind, need or memory the rules do not define (`check_expect`).
/// A kind means its family, `only` the kind alone, as in the rules.
pub fn expect(world: &mut World, e: &Expect) -> Result<(bool, String), String> {
    let kinds = world.resource::<Kinds>().clone();
    check_expect(&kinds, e)?;
    let family = |who: &Who| expect_family(&kinds, who);
    Ok(match e {
        Expect::Count { who, op, n } => {
            let ids = family(who)?;
            let c = world
                .query::<&ChunkActors>()
                .iter(world)
                .flat_map(|a| &a.rows)
                .filter(|r| ids.contains(&r.kind))
                .count() as i64;
            (op.holds(c, *n), c.to_string())
        }
        Expect::Tally {
            counter,
            who,
            op,
            n,
        } => {
            let tally = world.resource::<crate::actors::Tally>();
            let c: i64 = family(who)?.map(|k| tally.get(k, *counter) as i64).sum();
            (op.holds(c, *n), c.to_string())
        }
        Expect::Value {
            agg,
            name,
            who,
            op,
            v,
        } => {
            let ids = family(who)?;
            let slot = |k: u16| expect_slot(&kinds, k, name);
            // A decaying need as it stands now, as the next think will see it.
            let now = world.resource::<Tick>().0;
            let mut vals = Vec::new();
            for (a, m) in world.query::<(&ChunkActors, &ChunkMinds)>().iter(world) {
                for (r, mind) in a.rows.iter().zip(&m.rows) {
                    if ids.contains(&r.kind)
                        && let Some((need, i)) = slot(r.kind)
                    {
                        vals.push(i64::from(if need {
                            let decays = kinds.def(r.kind).needs[i].decays;
                            crate::rules::vm::need_now(mind.needs[i], decays, now, mind.last_think)
                        } else {
                            mind.mem[i]
                        }));
                    }
                }
            }
            let got = match agg {
                Agg::Min => vals.iter().min().copied(),
                Agg::Max => vals.iter().max().copied(),
                Agg::Sum => (!vals.is_empty()).then(|| vals.iter().sum()),
            };
            match got {
                Some(g) => (op.holds(g, *v), g.to_string()),
                None => (false, format!("no `{}` alive", who.kind)),
            }
        }
        Expect::At { x, y, who } => {
            let (cc, i) = Pos::new(*x, *y).split();
            let Some(cells) = stage::chunk(world, cc) else {
                return Ok((false, "that chunk is not loaded".into()));
            };
            let here = cells.occupant[i]
                .unpack()
                .or(cells.cover[i].unpack())
                .map(|(k, _)| k);
            let got = here.map_or("nobody".to_string(), |k| kinds.def(k).name.clone());
            let ok = match (who, here) {
                (None, None) => true,
                (Some(w), Some(k)) => family(w)?.contains(&k),
                (Some(w), None) => {
                    family(w)?;
                    false
                }
                (None, Some(_)) => false,
            };
            (ok, got)
        }
        Expect::Checksum(h) => {
            let c = checksum(world);
            (c == *h, format!("{c:016x}"))
        }
        Expect::State(h) => {
            let c = stage::checksum(world);
            (c == *h, format!("{c:016x}"))
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scenario::Scenario;
    use crate::sim::new_world;

    /// `expect` looks at the world as it is: counts by family or `only`,
    /// the life counters, a need over a kind, who stands where, checksums;
    /// what the rules lack is an error.
    #[test]
    fn expectations_read_the_world() {
        use crate::scenario::Check;
        let s = Scenario::parse(
            "t",
            "size 16 16
             terrain water_level 0 rock_on_soil 0 rock_on_water 0
             start chicken at (2, 2) with (food = 5h)
             start chick at (5, 5)
             start egg at (8, 8)
             expect count chicken == 2
             expect count only chicken == 1
             expect born chicken == 0
             expect min food of chicken == 5h
             expect max food of chicken == 1d
             expect sum food of only chick == 1d
             expect at (8, 8) egg
             expect at (8, 8) chicken
             expect at (9, 9) nobody
             expect count wolf == 0
             expect max sleep of chicken == 0
             expect max nectar of hive == 0
             expect sum food of fox == 0",
        )
        .unwrap();
        let mut w = new_world(&s);
        let got: Vec<Result<(bool, String), String>> = s
            .checks
            .iter()
            .map(|c| match c {
                Check::Expect { what, .. } => expect(&mut w, what),
                Check::Run(_) => unreachable!(),
            })
            .collect();
        let ok = |b: bool, v: &str| Ok((b, v.to_string()));
        let (h5, d1) = (crate::time::hours(5), crate::time::days(1));
        assert_eq!(
            got,
            [
                ok(true, "2"),
                ok(true, "1"),
                ok(true, "0"),
                ok(true, &h5.to_string()),
                ok(true, &d1.to_string()),
                ok(true, &d1.to_string()),
                ok(true, "egg"),
                ok(false, "egg"),
                ok(true, "nobody"),
                Err("the rules have no kind `wolf`".into()),
                Err("`chicken` has no need or memory `sleep`".into()),
                ok(false, "no `hive` alive"),
                ok(false, "no `fox` alive"),
            ]
        );
        let c = checksum(&mut w);
        assert_eq!(
            expect(&mut w, &Expect::Checksum(c)),
            ok(true, &format!("{c:016x}"))
        );
        assert!(!expect(&mut w, &Expect::State(c)).unwrap().0);
    }

    /// `check_expect` finds what an `expect` names that the rules lack,
    /// with no world: the errors `expect` gives when it reaches the line.
    #[test]
    fn expectations_are_checked_against_the_rules() {
        let kinds = Kinds::builtin();
        let who = |k: &str| Who {
            kind: k.into(),
            only: false,
        };
        let count = |k: &str| Expect::Count {
            who: who(k),
            op: crate::scenario::Op::Eq,
            n: 0,
        };
        let value = |name: &str, k: &str| Expect::Value {
            agg: Agg::Max,
            name: name.into(),
            who: who(k),
            op: crate::scenario::Op::Eq,
            v: 0,
        };
        assert_eq!(check_expect(&kinds, &count("fox")), Ok(()));
        assert_eq!(check_expect(&kinds, &value("food", "fox")), Ok(()));
        assert_eq!(
            check_expect(&kinds, &count("wolf")),
            Err("the rules have no kind `wolf`".into())
        );
        assert_eq!(
            check_expect(&kinds, &value("nn", "fox")),
            Err("`fox` has no need or memory `nn`".into())
        );
        let at = |w| Expect::At { x: 0, y: 0, who: w };
        assert_eq!(check_expect(&kinds, &at(None)), Ok(()));
        assert_eq!(
            check_expect(&kinds, &at(Some(who("wolf")))),
            Err("the rules have no kind `wolf`".into())
        );
    }
}
