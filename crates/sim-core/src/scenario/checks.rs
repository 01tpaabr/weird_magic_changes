//! A scenario test, run: its `run` lines step the world while every
//! expectation over a window (`always`, `never`, `eventually`, `mean`)
//! watches it, and each `expect` is decided at its line, in the order
//! written. `wmc scenario`, the docs' examples and the scenario fuzzer all
//! run a test through [`run_checks`].
//!
//! A window's checks fall at its start, at its end, and at every whole
//! minute of world time between (every 15 ticks): a condition that holds
//! for less than a minute between two checks can go unseen. A rate per day
//! reads its counter at the window's two ends.

use bevy_ecs::prelude::*;

use super::expect::{check_expect, expect, holds, measure};
use super::{Check, Expect, Over, Window, span};
use crate::rules::Kinds;
use crate::sim;
use crate::time::{TICKS_PER_DAY, TICKS_PER_MINUTE};

/// What [`run_checks`] reports, line by line.
#[derive(Debug, Clone, PartialEq)]
pub enum Event<'a> {
    /// A `run` line stepped the world this many ticks.
    Ran(u64),
    /// An `expect` line: whether it held and what it saw, or why it could
    /// not be checked (it names what the rules lack).
    Expected {
        line: u32,
        text: &'a str,
        result: Result<(bool, String), String>,
    },
}

/// `x` to two decimals, without trailing zeros: `0.03`, `24`, `2.5`.
fn decimal(x: f64) -> String {
    let s = format!("{x:.2}");
    s.trim_end_matches('0').trim_end_matches('.').to_string()
}

/// Is `t` (ticks since the world was made) one of `w`'s checks?
fn due(w: Window, t: u64) -> bool {
    (w.from..=w.to).contains(&t) && (t == w.from || t == w.to || t.is_multiple_of(TICKS_PER_MINUTE))
}

/// An expectation over a window, watching the run.
struct Watch<'a> {
    what: &'a Expect,
    window: Window,
    /// Checks made.
    checks: u64,
    /// What the rules lack, once found: the result.
    error: Option<String>,
    /// `always`, `never`, `eventually`: the first check that decides it
    /// (`always` false, `never` or `eventually` true), with what it saw.
    first: Option<(u64, String)>,
    /// The lowest and highest number read.
    range: Option<(i64, i64)>,
    /// `mean`: the sum of the numbers read, and how many there were.
    sum: f64,
    read: u64,
    /// `mean ... per day`: the counter at the window's start and end.
    at_from: Option<i64>,
    at_to: Option<i64>,
}

impl<'a> Watch<'a> {
    fn of(what: &'a Expect) -> Option<Self> {
        let window = match what {
            Expect::Over { window, .. } | Expect::Mean { window, .. } => *window,
            _ => return None,
        };
        Some(Self {
            what,
            window,
            checks: 0,
            error: None,
            first: None,
            range: None,
            sum: 0.0,
            read: 0,
            at_from: None,
            at_to: None,
        })
    }

    /// Does it look at the world `t` ticks after it was made?
    fn wants(&self, t: u64) -> bool {
        self.error.is_none()
            && match self.what {
                Expect::Mean { per_day: true, .. } => t == self.window.from || t == self.window.to,
                _ => due(self.window, t),
            }
    }

    fn widen(&mut self, n: Option<i64>) {
        if let Some(n) = n {
            self.range = Some(self.range.map_or((n, n), |(lo, hi)| (lo.min(n), hi.max(n))));
        }
    }

    fn look(&mut self, world: &World, kinds: &Kinds, t: u64) {
        if !self.wants(t) {
            return;
        }
        match self.what {
            Expect::Over { over, cond, .. } => {
                self.checks += 1;
                match holds(world, kinds, cond) {
                    Err(e) => self.error = Some(e),
                    Ok((ok, got, n)) => {
                        self.widen(n);
                        let decides = match over {
                            Over::Always => !ok,
                            Over::Never | Over::Eventually => ok,
                        };
                        if decides && self.first.is_none() {
                            self.first = Some((t, got));
                        }
                    }
                }
            }
            Expect::Mean {
                metric, per_day, ..
            } => match measure(world, kinds, metric) {
                Err(e) => self.error = Some(e),
                Ok(n) => {
                    self.checks += 1;
                    self.widen(n);
                    if *per_day {
                        if t == self.window.from {
                            self.at_from = n;
                        }
                        if t == self.window.to {
                            self.at_to = n;
                        }
                    } else if let Some(n) = n {
                        self.sum += n as f64;
                        self.read += 1;
                    }
                }
            },
            _ => {}
        }
    }

    /// The verdict, once the run has passed the window.
    fn result(&self) -> Result<(bool, String), String> {
        if let Some(e) = &self.error {
            return Err(e.clone());
        }
        let range = match self.range {
            Some((lo, hi)) if lo == hi => format!("; it was {lo}"),
            Some((lo, hi)) => format!("; from {lo} to {hi}"),
            None => String::new(),
        };
        let n = match self.checks {
            1 => "1 check".to_string(),
            n => format!("{n} checks"),
        };
        Ok(match self.what {
            Expect::Over { over, .. } => match (over, &self.first) {
                _ if self.checks == 0 => (false, "no check fell in the window".into()),
                (Over::Always, None) => (true, format!("true at all {n}{range}")),
                (Over::Always, Some((t, got))) => {
                    (false, format!("false at {} ({got}){range}", span(*t)))
                }
                (Over::Never, None) => (true, format!("false at all {n}{range}")),
                (Over::Never, Some((t, got))) => (false, format!("true at {} ({got})", span(*t))),
                (Over::Eventually, Some((t, got))) => {
                    (true, format!("first true at {} ({got})", span(*t)))
                }
                (Over::Eventually, None) => (false, format!("false at all {n}{range}")),
            },
            Expect::Mean {
                per_day: false,
                op,
                v,
                metric,
                ..
            } => {
                if self.read == 0 {
                    let who = match metric {
                        super::Metric::Count(w)
                        | super::Metric::Tally { who: w, .. }
                        | super::Metric::Value { who: w, .. } => w,
                    };
                    return Ok((false, format!("no `{}` alive at any check", who.kind)));
                }
                let mean = self.sum / self.read as f64;
                (
                    op.holds_f(mean, *v),
                    format!("mean {} over {n}{range}", decimal(mean)),
                )
            }
            Expect::Mean {
                per_day: true,
                op,
                v,
                ..
            } => {
                let (Some(a), Some(b)) = (self.at_from, self.at_to) else {
                    return Ok((false, "the run never reached the window's end".into()));
                };
                let w = self.window;
                let days = (w.to - w.from) as f64 / TICKS_PER_DAY as f64;
                let rate = (b - a) as f64 / days;
                (
                    op.holds_f(rate, *v),
                    format!(
                        "{} per day: {} in {}",
                        decimal(rate),
                        b - a,
                        span(w.to - w.from)
                    ),
                )
            }
            _ => unreachable!("a watch is over a window"),
        })
    }
}

/// Run a scenario test's `checks` on `world`, which its scenario has just
/// made (its windows count ticks from now): `run` lines step it while
/// every expectation over a window watches, and each `expect` is decided
/// at its line. `on` hears each line, in order. Never panics, whatever the
/// checks say: a window the run never reaches fails.
pub fn run_checks(world: &mut World, checks: &[Check], mut on: impl FnMut(Event<'_>)) {
    let kinds = world.resource::<Kinds>().clone();
    let mut watches: Vec<(usize, Watch)> = checks
        .iter()
        .enumerate()
        .filter_map(|(i, c)| match c {
            Check::Expect { what, .. } => Watch::of(what).map(|mut w| {
                w.error = check_expect(&kinds, what).err();
                (i, w)
            }),
            Check::Run(_) => None,
        })
        .collect();
    let mut t = 0u64;
    let look = |world: &World, watches: &mut Vec<(usize, Watch)>, t: u64| {
        for (_, w) in watches.iter_mut() {
            w.look(world, &kinds, t);
        }
    };
    look(world, &mut watches, t);
    for (i, c) in checks.iter().enumerate() {
        match c {
            Check::Run(n) => {
                for _ in 0..*n {
                    sim::step(world);
                    t += 1;
                    if watches.iter().any(|(_, w)| w.wants(t)) {
                        look(world, &mut watches, t);
                    }
                }
                on(Event::Ran(*n));
            }
            Check::Expect { line, text, what } => {
                let result = match watches.iter().find(|(j, _)| *j == i) {
                    Some((_, w)) => w.result(),
                    None => expect(world, what),
                };
                on(Event::Expected {
                    line: *line,
                    text,
                    result,
                });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rules::Kinds;
    use crate::scenario::Scenario;

    /// A starving fox penned with one hen: it eats her within the first
    /// hour (two bites), and nothing else happens.
    const PEN: &str = "seed 3
        outside rock
        map {
          .....
          .###.
          .FC#.
          .###.
        }
        legend {
          . soil
          # rock
          C chicken
          F fox with (food = 2h)
        }
        ";

    /// Each `expect` of `checks` (after the pen), in order: held, and what
    /// it saw; or the error.
    fn verdicts(checks: &str) -> Vec<Result<(bool, String), String>> {
        let s = Scenario::parse("pen", &format!("{PEN}{checks}")).unwrap();
        let mut world = sim::new_world_with(&s, Kinds::builtin()).unwrap();
        let mut got = Vec::new();
        run_checks(&mut world, &s.checks, |e| {
            if let Event::Expected { result, .. } = e {
                got.push(result);
            }
        });
        got
    }

    fn holds(r: &Result<(bool, String), String>) -> bool {
        matches!(r, Ok((true, _)))
    }

    #[test]
    fn windows_watch_the_run_and_decide_at_their_line() {
        let got = verdicts(
            "run 2h
             expect always count fox == 1
             expect eventually count chicken == 0
             expect never count chicken > 1
             expect always count chicken == 1
             expect eventually count fox == 2
             expect always count chicken == 0 from 1h
             expect mean count chicken < 0.1 from 1h
             expect mean eaten chicken by fox per day == 12
             expect mean eaten chicken by chicken per day == 0
             expect mean min food of chicken > 0 from 1h
             expect never died chicken of water > 0
             expect eventually at (2, 2) nobody
             expect count fox == 1",
        );
        let ok: Vec<bool> = got.iter().map(holds).collect();
        assert_eq!(
            ok,
            [
                true, true, true, false, false, true, true, true, true, false, true, true, true
            ],
            "{got:#?}"
        );
        let text = |i: usize| got[i].as_ref().unwrap().1.clone();
        assert_eq!(text(0), "true at all 121 checks; it was 1");
        assert!(text(1).starts_with("first true at "), "{}", text(1));
        // `always` says when it first failed, and what it saw then.
        assert!(
            text(3).starts_with("false at ") && text(3).contains("(0)"),
            "{}",
            text(3)
        );
        assert_eq!(text(4), "false at all 121 checks; it was 1");
        assert_eq!(text(7), "12 per day: 1 in 2h");
        assert_eq!(decimal(0.025), "0.03");
        assert_eq!(decimal(2.5), "2.5");
        assert_eq!(decimal(24.0), "24");
        assert_eq!(text(9), "no `chicken` alive at any check");
    }

    #[test]
    fn what_the_rules_lack_is_an_error_at_its_line() {
        let got = verdicts(
            "run 10
             expect always count wolf == 0
             expect mean eaten chicken by wolf per day >= 1",
        );
        assert_eq!(
            got,
            [
                Err("the rules have no kind `wolf`".into()),
                Err("the rules have no kind `wolf`".into()),
            ]
        );
    }

    /// Checks no longer than the windows they name, as a fuzzer may cut
    /// them: verdicts, never a panic.
    #[test]
    fn a_window_the_run_never_reaches_fails() {
        let s = Scenario::parse(
            "pen",
            &format!(
                "{PEN}run 1h\nexpect always count fox == 1\nexpect mean born fox per day >= 0"
            ),
        )
        .unwrap();
        let cut: Vec<Check> = s
            .checks
            .iter()
            .map(|c| match c {
                Check::Run(t) => Check::Run((*t).min(3)),
                c => c.clone(),
            })
            .collect();
        let mut world = sim::new_world_with(&s, Kinds::builtin()).unwrap();
        let mut got = Vec::new();
        run_checks(&mut world, &cut, |e| {
            if let Event::Expected { result, .. } = e {
                got.push(result);
            }
        });
        assert_eq!(got[0], Ok((true, "true at all 1 check; it was 1".into())));
        assert_eq!(
            got[1],
            Ok((false, "the run never reached the window's end".into()))
        );
    }
}
