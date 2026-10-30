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
use super::{Check, Expect, Op, Over, Window, span};
use crate::rules::Kinds;
use crate::sim;
use crate::time::{TICKS_PER_DAY, TICKS_PER_MINUTE};

/// What [`run_checks`] reports, line by line.
#[derive(Debug, Clone, PartialEq)]
pub enum Event<'a> {
    /// A `run` line stepped the world this many ticks.
    Ran(u64),
    /// An `expect` line: whether it held and what it saw, or why it could
    /// not be checked (it names what the rules lack). `value`: its key
    /// number, if it has one: what a moment's metric read; the extreme of
    /// a window that matters (the lowest an `always >=` saw, the highest a
    /// `never >` saw); a mean; a rate.
    Expected {
        line: u32,
        text: &'a str,
        result: Result<(bool, String), String>,
        value: Option<f64>,
    },
}

/// `x` to two decimals, without trailing zeros: `0.03`, `24`, `2.5`.
pub(super) fn decimal(x: f64) -> String {
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
    range: Option<(f64, f64)>,
    /// `mean`: the sum of the numbers read, and how many there were.
    sum: f64,
    read: u64,
    /// `mean ... per day`: the counter at the window's start and end.
    at_from: Option<f64>,
    at_to: Option<f64>,
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

    /// Is it already false, whatever the rest of the run does: an `always`
    /// false at a check, a `never` true, or what the rules lack?
    fn failed(&self) -> bool {
        self.error.is_some()
            || matches!(
                self.what,
                Expect::Over {
                    over: Over::Always | Over::Never,
                    ..
                }
            ) && self.first.is_some()
    }

    /// A rate's growth per day, once both ends of its window were read.
    fn rate(&self) -> Option<(f64, f64)> {
        let (a, b) = (self.at_from?, self.at_to?);
        let w = self.window;
        let days = (w.to - w.from) as f64 / TICKS_PER_DAY as f64;
        Some(((b - a) / days, b - a))
    }

    /// The key number ([`Event::Expected`]).
    fn value(&self) -> Option<f64> {
        match self.what {
            Expect::Over { over, cond, .. } => {
                let Expect::Is { op, .. } = cond.as_ref() else {
                    return None;
                };
                let (lo, hi) = self.range?;
                let up = matches!(op, Op::Ge | Op::Gt);
                let down = matches!(op, Op::Le | Op::Lt);
                let pick = match over {
                    Over::Always if up => lo,
                    Over::Always if down => hi,
                    Over::Never | Over::Eventually if up => hi,
                    Over::Never | Over::Eventually if down => lo,
                    _ => return None,
                };
                Some(pick)
            }
            Expect::Mean { per_day: false, .. } => {
                (self.read > 0).then(|| self.sum / self.read as f64)
            }
            Expect::Mean { per_day: true, .. } => self.rate().map(|(r, _)| r),
            _ => None,
        }
    }

    fn widen(&mut self, n: Option<f64>) {
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
                        self.sum += n;
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
            Some((lo, hi)) if lo == hi => format!("; it was {}", decimal(lo)),
            Some((lo, hi)) => format!("; from {} to {}", decimal(lo), decimal(hi)),
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
                let Some((rate, grew)) = self.rate() else {
                    return Ok((false, "the run never reached the window's end".into()));
                };
                let w = self.window;
                (
                    op.holds_f(rate, *v),
                    format!(
                        "{} per day: {} in {}",
                        decimal(rate),
                        decimal(grew),
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
pub fn run_checks(world: &mut World, checks: &[Check], on: impl FnMut(Event<'_>)) {
    run_checks_until(world, checks, false, on);
}

/// [`run_checks`]; with `stop`, the run ends at the first failure it can be
/// sure of (an `always` false, a `never` true, an `expect` of a moment that
/// fails, what the rules lack), and every `expect` after it is reported at
/// once: a window it decided with its verdict, the rest as not reached.
/// Returns whether it cut anything short (a `run`, or lines after the
/// failure). `wmc lab` stops a seed this way.
pub fn run_checks_until(
    world: &mut World,
    checks: &[Check],
    stop: bool,
    mut on: impl FnMut(Event<'_>),
) -> bool {
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
    let line_of = |i: usize| match &checks[i] {
        Check::Expect { line, .. } => *line,
        Check::Run(_) => 0,
    };
    // The first failure a watch is sure of: where the run stops.
    let failed = |watches: &[(usize, Watch)]| {
        watches
            .iter()
            .find(|(_, w)| w.failed())
            .map(|(i, _)| line_of(*i))
    };
    let mut t = 0u64;
    let look = |world: &World, watches: &mut Vec<(usize, Watch)>, t: u64| {
        for (_, w) in watches.iter_mut() {
            w.look(world, &kinds, t);
        }
    };
    look(world, &mut watches, t);
    let mut stopped: Option<(u64, u32)> = if stop {
        failed(&watches).map(|line| (t, line))
    } else {
        None
    };
    let mut cut = false;
    for (i, c) in checks.iter().enumerate() {
        if let Some((at, failing)) = stopped {
            cut = true;
            if let Check::Expect { line, text, .. } = c {
                let (result, value) = match watches.iter().find(|(j, _)| *j == i) {
                    Some((_, w)) if w.failed() => (w.result(), w.value()),
                    _ => (
                        Err(format!(
                            "not reached: the run stopped at {} when line {failing} failed",
                            span(at)
                        )),
                        None,
                    ),
                };
                on(Event::Expected {
                    line: *line,
                    text,
                    result,
                    value,
                });
            }
            continue;
        }
        match c {
            Check::Run(n) => {
                let mut ran = 0;
                for _ in 0..*n {
                    sim::step(world);
                    t += 1;
                    ran += 1;
                    if watches.iter().any(|(_, w)| w.wants(t)) {
                        look(world, &mut watches, t);
                        if stop && let Some(line) = failed(&watches) {
                            stopped = Some((t, line));
                            cut = ran < *n;
                            break;
                        }
                    }
                }
                on(Event::Ran(ran));
            }
            Check::Expect { line, text, what } => {
                let (result, value) = match watches.iter().find(|(j, _)| *j == i) {
                    Some((_, w)) => (w.result(), w.value()),
                    None => point(world, &kinds, what),
                };
                let held = matches!(result, Ok((true, _)));
                on(Event::Expected {
                    line: *line,
                    text,
                    result,
                    value,
                });
                if stop && !held {
                    stopped = Some((t, *line));
                }
            }
        }
    }
    cut
}

/// An expectation of a moment, decided now, with its number.
fn point(
    world: &mut World,
    kinds: &Kinds,
    e: &Expect,
) -> (Result<(bool, String), String>, Option<f64>) {
    match e {
        Expect::Is { .. } | Expect::At { .. } => match holds(world, kinds, e) {
            Ok((ok, got, n)) => (Ok((ok, got)), n),
            Err(e) => (Err(e), None),
        },
        _ => (expect(world, e), None),
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

    /// Each verdict's key number: the extreme of a window that matters, a
    /// mean, a rate, what a moment read; and a run that stops at its first
    /// sure failure reports the rest at once.
    #[test]
    fn verdicts_carry_their_number_and_a_run_can_stop_at_a_failure() {
        let s = Scenario::parse(
            "pen",
            &format!(
                "{PEN}run 2h
                 expect always count chicken >= 0
                 expect never count fox > 1
                 expect mean eaten chicken per day >= 1
                 expect count fox == 1
                 expect always count chicken == 1
                 run 1h
                 expect eventually count fox == 2
                 expect count fox == 1"
            ),
        )
        .unwrap();
        let run = |stop: bool| {
            let mut world = sim::new_world_with(&s, Kinds::builtin()).unwrap();
            let mut got = Vec::new();
            let mut ran = 0;
            let stopped = run_checks_until(&mut world, &s.checks, stop, |e| match e {
                Event::Expected { result, value, .. } => got.push((result, value)),
                Event::Ran(t) => ran += t,
            });
            (stopped, ran, got)
        };
        let (stopped, ran, got) = run(false);
        assert!(!stopped);
        assert_eq!(ran, 2 * 900 + 900);
        let values: Vec<Option<f64>> = got.iter().map(|(_, v)| *v).collect();
        // `always >=` keeps the lowest, `never >` the highest; a rate; a
        // count now; `always ==` has no direction.
        assert_eq!(
            values[..5],
            [Some(0.0), Some(1.0), Some(12.0), Some(1.0), None]
        );
        // Stopping: `always count chicken == 1` fails at the first minute
        // the hen is gone; the run ends there and the rest is not reached.
        let (stopped, ran, got) = run(true);
        assert!(stopped);
        assert!(ran < 2 * 900, "stopped within the first run: {ran}");
        assert_eq!(got.len(), 7, "every expect line is reported");
        assert!(matches!(&got[4].0, Ok((false, saw)) if saw.starts_with("false at ")));
        let failing = s
            .checks
            .iter()
            .filter_map(|c| match c {
                Check::Expect { line, .. } => Some(*line),
                Check::Run(_) => None,
            })
            .nth(4)
            .unwrap();
        for (result, value) in &got[..4] {
            let Err(e) = result else { panic!("{result:?}") };
            assert!(
                e.starts_with("not reached: the run stopped at ")
                    && e.ends_with(&format!("when line {failing} failed")),
                "{e}"
            );
            assert_eq!(*value, None);
        }
        assert!(matches!(&got[6].0, Err(e) if e.starts_with("not reached")));
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
