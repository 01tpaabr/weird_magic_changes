//! The scenario test's `expect` lines, evaluated: what [`expect`] finds in
//! a world now, what [`measure`] reads for a metric, and what
//! [`check_expect`] finds the rules lack without one. The parser is
//! [`super::Scenario::parse`]; expectations over a window are watched by
//! [`super::run_checks`]. `wmc scenario` and `wmc lint --scenario` call
//! these (as `sim::run_checks` and `sim::check_expect`).

use bevy_ecs::prelude::*;

use super::{Agg, CauseIs, Expect, Metric, Who};
use crate::actors::{Cause, ChunkActors, ChunkMinds, Tally};
use crate::rules::Kinds;
use crate::sim::{Tick, checksum};
use crate::stage::{self, Pos, Stage};

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

/// Whether the rules define every kind, need and memory `m` names.
fn check_metric(kinds: &Kinds, m: &Metric) -> Result<(), String> {
    match m {
        Metric::Count(who) => {
            expect_family(kinds, who)?;
        }
        Metric::Tally { who, cause, .. } => {
            let ids = expect_family(kinds, who)?;
            match cause {
                Some(CauseIs::By(by)) => {
                    expect_family(kinds, by)?;
                }
                Some(CauseIs::Of(need))
                    if !ids
                        .into_iter()
                        .any(|k| kinds.def(k).need_named(need).is_some()) =>
                {
                    return Err(format!("`{}` has no need `{need}`", who.kind));
                }
                Some(CauseIs::Of(_)) => {}
                None => {}
            }
        }
        Metric::Value { name, who, .. } => {
            if !expect_family(kinds, who)?.any(|k| expect_slot(kinds, k, name).is_some()) {
                return Err(format!("`{}` has no need or memory `{name}`", who.kind));
            }
        }
    }
    Ok(())
}

/// Whether the rules define every kind, need and memory `e` names, without
/// a world: `wmc lint --scenario` checks a test's `expect` lines with it.
/// The same errors `expect` gives.
pub fn check_expect(kinds: &Kinds, e: &Expect) -> Result<(), String> {
    match e {
        Expect::Is { metric, .. } | Expect::Mean { metric, .. } => check_metric(kinds, metric),
        Expect::Over { cond, .. } => check_expect(kinds, cond),
        Expect::At { who: Some(w), .. } => expect_family(kinds, w).map(|_| ()),
        Expect::At { who: None, .. } | Expect::Checksum(_) | Expect::State(_) => Ok(()),
    }
}

/// The number `m` reads from `world` now: `None` for a `min|max|sum` over a
/// kind with none alive. An error when it names what the rules lack. A kind
/// means its family, `only` the kind alone, as in the rules.
pub fn measure(world: &World, kinds: &Kinds, m: &Metric) -> Result<Option<i64>, String> {
    check_metric(kinds, m)?;
    let stage = world.resource::<Stage>();
    let rows = stage
        .active()
        .iter()
        .filter_map(|&(_, e)| Some((world.get::<ChunkActors>(e)?, world.get::<ChunkMinds>(e)?)));
    Ok(match m {
        Metric::Count(who) => {
            let ids = expect_family(kinds, who)?;
            let n: usize = rows
                .map(|(a, _)| a.rows.iter().filter(|r| ids.contains(&r.kind)).count())
                .sum();
            Some(i64::try_from(n).unwrap_or(i64::MAX))
        }
        Metric::Tally {
            counter,
            who,
            cause,
        } => {
            let tally = world.resource::<Tally>();
            let ids = expect_family(kinds, who)?;
            let n: u64 = match cause {
                None => ids.map(|k| tally.get(k, *counter)).sum(),
                Some(CauseIs::By(by)) => {
                    let killers = expect_family(kinds, by)?;
                    ids.flat_map(|k| tally.deaths_of(k))
                        .filter(|(c, _)| matches!(c, Cause::Killed(b) if killers.contains(b)))
                        .map(|(_, n)| n)
                        .sum()
                }
                Some(CauseIs::Of(need)) => ids
                    .filter_map(|k| {
                        let slot = u8::try_from(kinds.def(k).need_named(need)?).ok()?;
                        Some(
                            tally
                                .deaths_of(k)
                                .filter(|(c, _)| *c == Cause::Starved(slot))
                                .map(|(_, n)| n)
                                .sum::<u64>(),
                        )
                    })
                    .sum(),
            };
            Some(i64::try_from(n).unwrap_or(i64::MAX))
        }
        Metric::Value { agg, name, who } => {
            let ids = expect_family(kinds, who)?;
            let slot = |k: u16| expect_slot(kinds, k, name);
            // A decaying need as it stands now, as the next think will see it.
            let now = world.resource::<Tick>().0;
            let mut vals = Vec::new();
            for (a, m) in rows {
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
            match agg {
                Agg::Min => vals.iter().min().copied(),
                Agg::Max => vals.iter().max().copied(),
                Agg::Sum => (!vals.is_empty()).then(|| vals.iter().sum()),
            }
        }
    })
}

/// What a condition (`METRIC OP V`, `at ...`) finds in `world` now: whether
/// it holds, what it saw, and the number it read, if any.
pub fn holds(
    world: &World,
    kinds: &Kinds,
    e: &Expect,
) -> Result<(bool, String, Option<i64>), String> {
    match e {
        Expect::Is { metric, op, v } => Ok(match measure(world, kinds, metric)? {
            Some(n) => (op.holds(n, *v), n.to_string(), Some(n)),
            None => {
                let who = match metric {
                    Metric::Count(w)
                    | Metric::Tally { who: w, .. }
                    | Metric::Value { who: w, .. } => w,
                };
                (false, format!("no `{}` alive", who.kind), None)
            }
        }),
        Expect::At { x, y, who } => {
            let (cc, i) = Pos::new(*x, *y).split();
            let Some(cells) = stage::chunk(world, cc) else {
                return Ok((false, "that chunk is not loaded".into(), None));
            };
            let here = cells.occupant[i]
                .unpack()
                .or(cells.cover[i].unpack())
                .map(|(k, _)| k);
            let got = here.map_or("nobody".to_string(), |k| kinds.def(k).name.clone());
            let ok = match (who, here) {
                (None, None) => true,
                (Some(w), Some(k)) => expect_family(kinds, w)?.contains(&k),
                (Some(w), None) => {
                    expect_family(kinds, w)?;
                    false
                }
                (None, Some(_)) => false,
            };
            Ok((ok, got, None))
        }
        Expect::Checksum(_) | Expect::State(_) | Expect::Over { .. } | Expect::Mean { .. } => {
            Err("not a condition of one moment: `METRIC OP V` or `at ...`".into())
        }
    }
}

/// What a scenario test's `expect` finds in `world` now (`wmc scenario`):
/// whether it holds, and what it saw, for the report. An error when it
/// names a kind, need or memory the rules do not define (`check_expect`),
/// or looks at a window: [`super::run_checks`] watches those.
pub fn expect(world: &mut World, e: &Expect) -> Result<(bool, String), String> {
    match e {
        Expect::Checksum(h) => {
            let c = checksum(world);
            Ok((c == *h, format!("{c:016x}")))
        }
        Expect::State(h) => {
            let c = stage::checksum(world);
            Ok((c == *h, format!("{c:016x}")))
        }
        Expect::Over { .. } | Expect::Mean { .. } => Err(
            "`always`, `never`, `eventually` and `mean` look at a run: check them with their scenario (`wmc scenario`)"
                .into(),
        ),
        Expect::Is { .. } | Expect::At { .. } => {
            let kinds = world.resource::<Kinds>();
            holds(world, kinds, e).map(|(ok, got, _)| (ok, got))
        }
    }
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
        let count = |k: &str| Expect::Is {
            metric: Metric::Count(who(k)),
            op: crate::scenario::Op::Eq,
            v: 0,
        };
        let value = |name: &str, k: &str| Expect::Is {
            metric: Metric::Value {
                agg: Agg::Max,
                name: name.into(),
                who: who(k),
            },
            op: crate::scenario::Op::Eq,
            v: 0,
        };
        let died_of = |need: &str| Expect::Is {
            metric: Metric::Tally {
                counter: crate::actors::life::DIED,
                who: who("chicken"),
                cause: Some(CauseIs::Of(need.into())),
            },
            op: crate::scenario::Op::Eq,
            v: 0,
        };
        let eaten_by = |k: &str| Expect::Is {
            metric: Metric::Tally {
                counter: crate::actors::life::EATEN,
                who: who("chicken"),
                cause: Some(CauseIs::By(who(k))),
            },
            op: crate::scenario::Op::Eq,
            v: 0,
        };
        assert_eq!(check_expect(&kinds, &died_of("water")), Ok(()));
        assert_eq!(
            check_expect(&kinds, &died_of("nectar")),
            Err("`chicken` has no need `nectar`".into())
        );
        assert_eq!(check_expect(&kinds, &eaten_by("fox")), Ok(()));
        assert_eq!(
            check_expect(&kinds, &eaten_by("wolf")),
            Err("the rules have no kind `wolf`".into())
        );
        let over = Expect::Over {
            over: crate::scenario::Over::Always,
            cond: Box::new(count("wolf")),
            window: crate::scenario::Window { from: 0, to: 9 },
        };
        assert_eq!(
            check_expect(&kinds, &over),
            Err("the rules have no kind `wolf`".into())
        );
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
