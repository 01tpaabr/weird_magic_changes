//! `wmc observe`: how a world's populations move over time, and why. The
//! middle zoom level between `wmc why` (one actor's next think) and `wmc
//! run` (totals at the end), for tuning a world's dynamics.
//!
//! Every `every` ticks it samples each kind: how many are alive; births and
//! deaths since the last sample, deaths by cause (eaten by which kind,
//! starved of which need, or a rule's own `die`); its needs over the alive
//! (min, p10, median, p90, max, mean, decayed to now); how many are in each
//! state; the last results of their actions; and, for up to `sample` of
//! them, what their next think would decide and, per rule, whether it was
//! reached, fired, or which term of its condition stopped it, with where
//! one such actor stood (a dry run, as the inspector does: nothing in the
//! world changes).
//!
//! The samples go to a long CSV (`tick,day,hour,kind,metric,value`) and a
//! one-screen summary. Everything walks the chunks in coordinate order and
//! picks the sampled actors by a hash of uid and tick, so the output is the
//! same on any thread count.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::io::{self, Write};

use bevy::prelude::World;
use sim_core::actors::{Cause, Tally, life};
use sim_core::rng::splitmix64;
use sim_core::rules::vm::{Action, need_now, result};
use sim_core::time::{Clock, TICKS_PER_DAY, TICKS_PER_HOUR};
use sim_core::{ChunkActors, ChunkCoord, ChunkMinds, Kinds, Stage, sim};

use crate::why::{checked_rules, duration, state_name};

/// The life events a sample counts, with their CSV names.
const EVENTS: [(usize, &str); 6] = [
    (life::BORN, "born"),
    (life::BECAME, "became"),
    (life::EATEN, "eaten"),
    (life::DIED, "died"),
    (life::THINKS, "thinks"),
    (life::TRAPS, "traps"),
];

/// Last-result names, by result code.
const RESULTS: [&str; 5] = ["none", "ok", "blocked", "missed", "refused"];

/// How often to sample, and how many thinks to explain per kind each time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Options {
    /// Ticks between samples, at least 1.
    pub every: u64,
    /// Actors per kind whose next think is explained at each sample.
    pub sample: usize,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            every: TICKS_PER_HOUR,
            sample: 100,
        }
    }
}

/// A need over the alive of a kind at one sample, decayed to now.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct NeedStats {
    pub min: i32,
    pub p10: i32,
    pub p50: i32,
    pub p90: i32,
    pub max: i32,
    pub mean: f64,
}

/// One kind at one sample.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct KindSample {
    pub alive: u64,
    /// [`EVENTS`] since the last sample.
    pub events: [u64; EVENTS.len()],
    /// Deaths since the last sample, by cause.
    pub deaths: BTreeMap<Cause, u64>,
    /// Per need of the kind; empty when none is alive.
    pub needs: Vec<NeedStats>,
    /// Alive per state, in state order.
    pub states: Vec<u64>,
    /// Alive by the result of their last action, by result code.
    pub results: [u64; RESULTS.len()],
    /// Thinks explained.
    pub sampled: u64,
    /// What those thinks decide: an action, `starved` or `trapped`.
    pub decisions: BTreeMap<&'static str, u64>,
    /// The rules those thinks reached, by `file:line`.
    pub rules: BTreeMap<String, RuleTally>,
}

/// Where an explained think stood, and when: (x, y, tick).
pub type Seen = (i32, i32, u64);

/// One rule over the thinks a sample explained.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct RuleTally {
    /// Thinks that checked its condition, and that ran its body.
    pub reached: u64,
    pub fired: u64,
    /// Per term of its condition (`RuleInfo::terms`), the thinks it stopped:
    /// false there, or trapped.
    pub stopped: Vec<u64>,
    /// The first think that fired it, and per term the first it stopped.
    pub fired_at: Option<Seen>,
    pub stopped_at: Vec<Option<Seen>>,
}

/// A rule's source: its text, from `when` to `=>`, and its terms.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct RuleSource {
    pub text: String,
    pub terms: Vec<String>,
}

/// Every kind at one tick.
#[derive(Debug, Clone, PartialEq)]
pub struct Sample {
    pub tick: u64,
    pub kinds: Vec<KindSample>,
}

/// A whole observation: the samples, from the first tick to the last, and
/// the source of every rule they name.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Report {
    pub samples: Vec<Sample>,
    pub rules: BTreeMap<String, RuleSource>,
}

/// Step `world` for `ticks`, sampling at the start, every `opts.every`
/// ticks, and at the end.
pub fn observe(world: &mut World, ticks: u64, opts: Options) -> Report {
    let every = opts.every.max(1);
    let mut report = Report::default();
    let mut seen = world.resource::<Tally>().clone();
    report
        .samples
        .push(sample(world, opts, &seen, &mut report.rules));
    let mut done = 0;
    while done < ticks {
        let n = every.min(ticks - done);
        for _ in 0..n {
            sim::step(world);
        }
        done += n;
        report
            .samples
            .push(sample(world, opts, &seen, &mut report.rules));
        seen = world.resource::<Tally>().clone();
    }
    report
}

/// One sample of `world`, with life events counted since `seen`.
fn sample(
    world: &World,
    opts: Options,
    seen: &Tally,
    sources: &mut BTreeMap<String, RuleSource>,
) -> Sample {
    let kinds = world.resource::<Kinds>();
    let tally = world.resource::<Tally>();
    let tick = sim::tick(world);
    let n = kinds.defs.len();
    let mut out: Vec<KindSample> = vec![KindSample::default(); n];
    for (k, ks) in out.iter_mut().enumerate() {
        let kind = u16::try_from(k).expect("kind ids are u16");
        for (slot, &(event, _)) in EVENTS.iter().enumerate() {
            ks.events[slot] = tally.get(kind, event) - seen.get(kind, event);
        }
        for (cause, total) in tally.deaths_of(kind) {
            let before = seen.deaths.get(&(kind, cause)).copied().unwrap_or(0);
            if total > before {
                ks.deaths.insert(cause, total - before);
            }
        }
        ks.states = vec![0; usize::from(kinds.defs[k].states.max(1))];
    }

    // Every alive row: needs, states, last results.
    let mut needs: Vec<Vec<Vec<i32>>> = kinds
        .defs
        .iter()
        .map(|d| vec![Vec::new(); d.needs.len()])
        .collect();
    let stage = world.resource::<Stage>();
    let chunks: Vec<(ChunkCoord, &ChunkActors, &ChunkMinds)> = stage
        .active()
        .iter()
        .filter_map(|&(c, e)| Some((c, world.get::<ChunkActors>(e)?, world.get::<ChunkMinds>(e)?)))
        .collect();
    for (_, pubs, minds) in &chunks {
        for (row, mind) in pubs.rows.iter().zip(&minds.rows) {
            let k = usize::from(row.kind);
            let Some(ks) = out.get_mut(k) else { continue };
            ks.alive += 1;
            for (i, need) in kinds.defs[k].needs.iter().enumerate() {
                needs[k][i].push(need_now(mind.needs[i], need.decays, tick, mind.last_think));
            }
            if let Some(s) = ks.states.get_mut(usize::from(mind.state)) {
                *s += 1;
            }
            let code = usize::from(mind.events & result::MASK);
            ks.results[if code < RESULTS.len() { code } else { 0 }] += 1;
        }
    }
    for (ks, values) in out.iter_mut().zip(&mut needs) {
        ks.needs = values
            .iter_mut()
            .filter(|v| !v.is_empty())
            .map(|v| stats(v))
            .collect();
    }

    // Up to `opts.sample` per kind, picked by a hash of uid and tick: what
    // their next think would decide, and the rules it would fire.
    let cap = opts.sample as u64;
    for (cc, pubs, minds) in &chunks {
        let slots: Vec<u16> = pubs
            .rows
            .iter()
            .zip(&minds.rows)
            .enumerate()
            .filter(|(_, (row, mind))| {
                let alive = out.get(usize::from(row.kind)).map_or(0, |k| k.alive);
                alive <= cap || splitmix64(mind.uid ^ splitmix64(tick)) % alive < cap
            })
            .map(|(i, _)| sim_core::actors::slot_u16(i))
            .collect();
        if slots.is_empty() {
            continue;
        }
        for e in sim::explain_slots(world, *cc, &slots).unwrap_or_default() {
            let Some(ks) = out.get_mut(usize::from(e.row.kind)) else {
                continue;
            };
            ks.sampled += 1;
            let decision = if e.starved {
                "starved"
            } else if e.outcome.trap.is_some() {
                "trapped"
            } else {
                action_name(e.intent.action)
            };
            *ks.decisions.entry(decision).or_default() += 1;
            let at = cc.cell(usize::from(e.row.cell));
            let seen = (at.x, at.y, tick);
            for c in checked_rules(kinds, &e)
                .iter()
                .filter(|c| !c.verdict.is_empty())
            {
                let key = format!("{}:{}", c.file, c.rule.line);
                let terms = c.rule.terms.len();
                sources.entry(key.clone()).or_insert_with(|| RuleSource {
                    text: c.rule.text.clone(),
                    terms: c.rule.terms.iter().map(|t| t.text.clone()).collect(),
                });
                let t = ks.rules.entry(key).or_insert_with(|| RuleTally {
                    stopped: vec![0; terms],
                    stopped_at: vec![None; terms],
                    ..RuleTally::default()
                });
                t.reached += 1;
                if c.verdict == "FIRED" {
                    t.fired += 1;
                    t.fired_at.get_or_insert(seen);
                } else if let Some(i) = c.failed.filter(|&i| i < terms) {
                    t.stopped[i] += 1;
                    t.stopped_at[i].get_or_insert(seen);
                }
            }
        }
    }
    Sample { tick, kinds: out }
}

fn action_name(a: Action) -> &'static str {
    match a {
        Action::Idle => "idle",
        Action::Die => "die",
        Action::Become => "become",
        Action::Spawn => "spawn",
        Action::Move => "move",
        Action::Drink => "drink",
        Action::Eat => "eat",
        Action::Hit => "hit",
        Action::Graze => "graze",
        Action::Take => "take",
        Action::Give => "give",
    }
}

/// Order statistics and the mean of `v` (not empty; reordered).
fn stats(v: &mut [i32]) -> NeedStats {
    let n = v.len();
    let mut at = |p: usize| *v.select_nth_unstable((n - 1) * p / 100).1;
    let (min, p10, p50, p90, max) = (at(0), at(10), at(50), at(90), at(100));
    let sum: i64 = v.iter().map(|&x| i64::from(x)).sum();
    NeedStats {
        min,
        p10,
        p50,
        p90,
        max,
        mean: sum as f64 / n as f64,
    }
}

/// How a cause of death reads: the killer's kind, the need that ran out,
/// or `own die`.
fn cause_name(kinds: &Kinds, kind: u16, c: Cause) -> String {
    match c {
        Cause::Killed(by) => format!(
            "eaten by {}",
            kinds
                .defs
                .get(usize::from(by))
                .map_or("?", |d| d.name.as_str())
        ),
        Cause::Starved(i) => format!(
            "no {}",
            kinds
                .def(kind)
                .needs
                .get(usize::from(i))
                .map_or("?", |n| n.name.as_str())
        ),
        Cause::Died => "own die".into(),
    }
}

/// The CSV name of a cause: `killed.fox`, `starved.water`, `died`.
fn cause_key(kinds: &Kinds, kind: u16, c: Cause) -> String {
    match c {
        Cause::Killed(by) => format!(
            "killed.{}",
            kinds
                .defs
                .get(usize::from(by))
                .map_or("?", |d| d.name.as_str())
        ),
        Cause::Starved(i) => format!(
            "starved.{}",
            kinds
                .def(kind)
                .needs
                .get(usize::from(i))
                .map_or("?", |n| n.name.as_str())
        ),
        Cause::Died => "died".into(),
    }
}

/// A CSV field: quoted when it holds a comma, a quote or a line break.
pub(crate) fn field(s: &str) -> String {
    if s.contains([',', '"', '\n', '\r']) {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s.to_string()
    }
}

/// The report as a long CSV: one row per sample, kind and metric.
pub fn write_csv(report: &Report, kinds: &Kinds, out: &mut impl Write) -> io::Result<()> {
    writeln!(out, "tick,day,hour,kind,metric,value")?;
    for s in &report.samples {
        let c = Clock::at(s.tick);
        for (k, ks) in s.kinds.iter().enumerate() {
            let def = &kinds.defs[k];
            let kind = u16::try_from(k).expect("kind ids are u16");
            let name = field(&def.name);
            let mut row = |metric: &str, value: String| {
                writeln!(
                    out,
                    "{},{},{},{name},{},{value}",
                    s.tick,
                    c.day,
                    c.hour,
                    field(metric)
                )
            };
            row("alive", ks.alive.to_string())?;
            for (slot, (_, label)) in EVENTS.iter().enumerate() {
                row(label, ks.events[slot].to_string())?;
            }
            for (&cause, n) in &ks.deaths {
                row(
                    &format!("death.{}", cause_key(kinds, kind, cause)),
                    n.to_string(),
                )?;
            }
            for (need, st) in def.needs.iter().zip(&ks.needs) {
                for (stat, v) in [
                    ("min", st.min),
                    ("p10", st.p10),
                    ("p50", st.p50),
                    ("p90", st.p90),
                    ("max", st.max),
                ] {
                    row(&format!("need.{}.{stat}", need.name), v.to_string())?;
                }
                row(
                    &format!("need.{}.mean", need.name),
                    format!("{:.1}", st.mean),
                )?;
            }
            if def.states > 1 {
                for (i, &n) in ks.states.iter().enumerate() {
                    let state = state_name(kinds, kind, u8::try_from(i).expect("64 states"));
                    row(&format!("state.{state}"), n.to_string())?;
                }
            }
            for (code, &n) in ks.results.iter().enumerate() {
                if n > 0 {
                    row(&format!("result.{}", RESULTS[code]), n.to_string())?;
                }
            }
            row("sampled", ks.sampled.to_string())?;
            for (d, n) in &ks.decisions {
                row(&format!("decide.{d}"), n.to_string())?;
            }
            for (r, t) in &ks.rules {
                row(&format!("rule.{r}"), t.fired.to_string())?;
                row(&format!("rule.{r}.reached"), t.reached.to_string())?;
                for (i, n) in t.stopped.iter().enumerate() {
                    row(&format!("rule.{r}.term{}", i + 1), n.to_string())?;
                }
            }
        }
    }
    Ok(())
}

/// A need's value as people read it: time for a need that runs down,
/// points for one that does not.
fn need_value(decays: bool, v: f64) -> String {
    if decays {
        // A need holds at most i32::MAX ticks.
        #[allow(clippy::cast_possible_truncation)]
        duration(v.round() as i64)
    } else {
        format!("{}", v.round())
    }
}

/// `n` of `total` as a whole percentage; `<1%` for some that rounds to 0.
fn pct(n: u64, total: u64) -> String {
    match (n * 100 + total / 2) / total.max(1) {
        0 if n > 0 => "<1%".into(),
        p => format!("{p}%"),
    }
}

/// The one-screen summary: alive over time, deaths and births per day by
/// cause, then, per kind (all, or those named in `only`), its typical
/// needs, states, what it decides, the rules that fire most and how its
/// actions turn out.
pub fn summary(report: &Report, kinds: &Kinds, only: &[String]) -> String {
    let mut s = String::new();
    let (Some(first), Some(last)) = (report.samples.first(), report.samples.last()) else {
        return s;
    };
    let span = last.tick - first.tick;
    let days = span as f64 / TICKS_PER_DAY as f64;
    let names: Vec<&str> = kinds.defs.iter().map(|d| d.name.as_str()).collect();

    // Alive over time: about 16 rows, on whole sample steps.
    let step_len = report
        .samples
        .get(1)
        .map_or(1, |s| (s.tick - first.tick).max(1));
    let steps = report.samples.len() - 1;
    let stride = steps.div_ceil(16).max(1);
    let width = names.iter().map(|n| n.len()).max().unwrap_or(5).max(6) + 1;
    let _ = write!(s, "alive          ");
    for n in &names {
        let _ = write!(s, "{n:>width$}");
    }
    let _ = writeln!(s);
    let rows = (0..=steps)
        .step_by(stride)
        .chain((!steps.is_multiple_of(stride)).then_some(steps));
    for i in rows {
        let sm = &report.samples[i];
        let _ = write!(s, "{:<15}", Clock::at(sm.tick).to_string());
        for ks in &sm.kinds {
            let _ = write!(s, "{:>width$}", ks.alive);
        }
        let _ = writeln!(s);
    }
    let _ = write!(s, "{:<15}", "lowest");
    for k in 0..names.len() {
        let min = report
            .samples
            .iter()
            .map(|sm| sm.kinds[k].alive)
            .min()
            .unwrap_or(0);
        let _ = write!(s, "{min:>width$}");
    }
    let _ = writeln!(s);
    let _ = writeln!(
        s,
        "(every {} of {})",
        duration(i64::try_from(step_len * stride as u64).unwrap_or(i64::MAX)),
        duration(i64::try_from(span).unwrap_or(i64::MAX))
    );

    // Births and deaths per day, deaths by cause; per head: of the mean
    // alive over the samples.
    let mean_alive = |k: usize| {
        report
            .samples
            .iter()
            .map(|sm| sm.kinds[k].alive)
            .sum::<u64>() as f64
            / report.samples.len() as f64
    };
    let per_day = |n: u64| {
        if days > 0.0 {
            n as f64 / days
        } else {
            n as f64
        }
    };
    let _ = writeln!(
        s,
        "\nper day          born  became    died  per head  causes of death"
    );
    for (k, name) in names.iter().enumerate() {
        let kind = u16::try_from(k).expect("kind ids are u16");
        let sum = |slot: usize| -> u64 {
            report
                .samples
                .iter()
                .map(|sm| sm.kinds[k].events[slot])
                .sum()
        };
        let (born, became) = (sum(0), sum(1));
        let mut causes: BTreeMap<Cause, u64> = BTreeMap::new();
        for sm in &report.samples {
            for (&c, &n) in &sm.kinds[k].deaths {
                *causes.entry(c).or_default() += n;
            }
        }
        let dead: u64 = causes.values().sum();
        if born + became + dead == 0 {
            continue;
        }
        let rate = |n: u64| {
            if days > 0.0 {
                format!("{:.1}", n as f64 / days)
            } else {
                n.to_string()
            }
        };
        let mut ranked: Vec<(Cause, u64)> = causes.into_iter().collect();
        ranked.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
        let why: Vec<String> = ranked
            .iter()
            .map(|&(c, n)| format!("{} {}", cause_name(kinds, kind, c), pct(n, dead)))
            .collect();
        let head = mean_alive(k);
        let _ = writeln!(
            s,
            "{name:<14}{:>7} {:>7} {:>7} {:>9}  {}",
            rate(born),
            rate(became),
            rate(dead),
            if head > 0.0 {
                format!("{:.3}", per_day(dead) / head)
            } else {
                "-".into()
            },
            why.join(", ")
        );
    }

    // Who kills whom: per killer kind, kills a day, per head, and of what.
    let mut kills: BTreeMap<u16, BTreeMap<usize, u64>> = BTreeMap::new();
    for sm in &report.samples {
        for (victim, ks) in sm.kinds.iter().enumerate() {
            for (&c, &n) in &ks.deaths {
                if let Cause::Killed(by) = c {
                    *kills.entry(by).or_default().entry(victim).or_default() += n;
                }
            }
        }
    }
    if !kills.is_empty() {
        let _ = writeln!(s, "\nkills a day       all  per head  of");
        for (by, victims) in &kills {
            let k = usize::from(*by);
            let Some(name) = names.get(k) else { continue };
            let all: u64 = victims.values().sum();
            let head = mean_alive(k);
            let of: Vec<String> = victims
                .iter()
                .map(|(&v, &n)| format!("{} {:.1}", names[v], per_day(n)))
                .collect();
            let _ = writeln!(
                s,
                "{name:<14}{:>7.1} {:>9}  {}",
                per_day(all),
                if head > 0.0 {
                    format!("{:.2}", per_day(all) / head)
                } else {
                    "-".into()
                },
                of.join(", ")
            );
        }
    }

    // Per kind, over every sample.
    let _ = writeln!(
        s,
        "\nper kind, over the run (needs: median of each sample, p10-p90)"
    );
    for (k, name) in names.iter().enumerate() {
        if !only.is_empty() && !only.iter().any(|o| o == name) {
            continue;
        }
        let def = &kinds.defs[k];
        let kind = u16::try_from(k).expect("kind ids are u16");
        let lived: Vec<&KindSample> = report
            .samples
            .iter()
            .map(|sm| &sm.kinds[k])
            .filter(|ks| ks.alive > 0)
            .collect();
        if lived.is_empty() {
            continue;
        }
        let _ = writeln!(s, "{name}");
        if !def.needs.is_empty() {
            let parts: Vec<String> = def
                .needs
                .iter()
                .enumerate()
                .map(|(i, need)| {
                    let avg = |f: fn(&NeedStats) -> i32| {
                        lived
                            .iter()
                            .map(|ks| f64::from(f(&ks.needs[i])))
                            .sum::<f64>()
                            / lived.len() as f64
                    };
                    format!(
                        "{} {} ({}-{})",
                        need.name,
                        need_value(need.decays, avg(|n| n.p50)),
                        need_value(need.decays, avg(|n| n.p10)),
                        need_value(need.decays, avg(|n| n.p90)),
                    )
                })
                .collect();
            let _ = writeln!(s, "  needs    {}", parts.join("  "));
        }
        if def.states > 1 {
            let total: u64 = lived.iter().map(|ks| ks.states.iter().sum::<u64>()).sum();
            let parts: Vec<String> = (0..usize::from(def.states))
                .map(|i| {
                    let n: u64 = lived
                        .iter()
                        .map(|ks| ks.states.get(i).copied().unwrap_or(0))
                        .sum();
                    let state = state_name(kinds, kind, u8::try_from(i).expect("64 states"));
                    format!("{state} {}", pct(n, total))
                })
                .collect();
            let _ = writeln!(s, "  states   {}", parts.join("  "));
        }
        let sampled: u64 = lived.iter().map(|ks| ks.sampled).sum();
        if sampled > 0 {
            let mut decisions: BTreeMap<&str, u64> = BTreeMap::new();
            let mut rules: BTreeMap<&str, u64> = BTreeMap::new();
            for ks in &lived {
                for (&d, &n) in &ks.decisions {
                    *decisions.entry(d).or_default() += n;
                }
                for (r, t) in &ks.rules {
                    if t.fired > 0 {
                        *rules.entry(r.as_str()).or_default() += t.fired;
                    }
                }
            }
            let mut ranked: Vec<(&str, u64)> = decisions.into_iter().collect();
            ranked.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));
            let parts: Vec<String> = ranked
                .iter()
                .map(|&(d, n)| format!("{d} {}", pct(n, sampled)))
                .collect();
            let _ = writeln!(s, "  decides  {}", parts.join("  "));
            let mut ranked: Vec<(&str, u64)> = rules.into_iter().collect();
            ranked.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));
            for (i, (r, n)) in ranked.iter().take(5).enumerate() {
                // The source's alignment, collapsed.
                let text = report.rules.get(*r).map_or(String::new(), |t| {
                    t.text.split_whitespace().collect::<Vec<_>>().join(" ")
                });
                let label = if i == 0 { "  fires" } else { "" };
                let _ = writeln!(s, "{label:<9}{:>5}  {text}  [{r}]", pct(*n, sampled));
            }
        }
        let mut results = [0u64; RESULTS.len()];
        for ks in &lived {
            for (t, n) in results.iter_mut().zip(ks.results) {
                *t += n;
            }
        }
        let acted: u64 = results[1..].iter().sum();
        if acted > 0 {
            let parts: Vec<String> = (1..RESULTS.len())
                .filter(|&c| results[c] > 0)
                .map(|c| format!("{} {}", RESULTS[c].to_uppercase(), pct(results[c], acted)))
                .collect();
            let _ = writeln!(s, "  results  {}", parts.join("  "));
        }
        let traps: u64 = report.samples.iter().map(|sm| sm.kinds[k].events[5]).sum();
        if traps > 0 {
            let _ = writeln!(s, "  TRAPS    {traps} thinks trapped (`wmc why` shows one)");
        }
    }
    s
}

/// Per kind (all, or those named in `only`), each of its rules the
/// explained thinks reached, in code order: how many reached it and fired
/// it, and per term of its condition how many of the thinks that got that
/// far it stopped, each with where one such actor stood (`wmc why` it at
/// that time to see the think). The term that stops most of the thinks
/// reaching it is the gate that binds.
pub fn rule_report(report: &Report, kinds: &Kinds, only: &[String]) -> String {
    let mut s = String::new();
    let Some(first) = report.samples.first() else {
        return s;
    };
    let seen = |at: Option<Seen>| {
        at.map_or(String::new(), |(x, y, t)| {
            format!("   e.g. ({x}, {y}) after {} ticks", t - first.tick)
        })
    };
    for (k, def) in kinds.defs.iter().enumerate() {
        if !only.is_empty() && !only.contains(&def.name) {
            continue;
        }
        let kind = u16::try_from(k).expect("kind ids are u16");
        // Over every sample, by `file:line`.
        let mut total: BTreeMap<&str, RuleTally> = BTreeMap::new();
        let mut sampled = 0;
        for sm in &report.samples {
            let ks = &sm.kinds[k];
            sampled += ks.sampled;
            for (key, t) in &ks.rules {
                let sum = total.entry(key.as_str()).or_insert_with(|| RuleTally {
                    stopped: vec![0; t.stopped.len()],
                    stopped_at: vec![None; t.stopped.len()],
                    ..RuleTally::default()
                });
                sum.reached += t.reached;
                sum.fired += t.fired;
                sum.fired_at = sum.fired_at.or(t.fired_at);
                for (i, (&n, &at)) in t.stopped.iter().zip(&t.stopped_at).enumerate() {
                    if let Some(x) = sum.stopped.get_mut(i) {
                        *x += n;
                        sum.stopped_at[i] = sum.stopped_at[i].or(at);
                    }
                }
            }
        }
        if total.is_empty() {
            continue;
        }
        let _ = writeln!(
            s,
            "{} rules, over {sampled} explained thinks: reached, fired, and per term the thinks it stopped of those that got to it",
            def.name
        );
        let files = &kinds.debug.files;
        let mut shown = Vec::new();
        for r in kinds.debug.rules.iter().filter(|r| r.kind == kind) {
            let file = files.get(usize::from(r.file)).map_or("?", String::as_str);
            let key = format!("{file}:{}", r.line);
            let Some(t) = total.get(key.as_str()) else {
                continue;
            };
            if shown.contains(&key) {
                continue;
            }
            let src = report.rules.get(&key).cloned().unwrap_or_default();
            let text: Vec<&str> = src.text.split_whitespace().collect();
            let _ = writeln!(s, "  {key}  {}", text.join(" "));
            let _ = writeln!(
                s,
                "    reached {} ({}), fired {} ({} of those){}",
                t.reached,
                pct(t.reached, sampled),
                t.fired,
                pct(t.fired, t.reached),
                seen(t.fired_at)
            );
            if src.terms.len() > 1 {
                let width = src.terms.iter().map(String::len).max().unwrap_or(0).min(48);
                let mut got = t.reached;
                for (i, term) in src.terms.iter().enumerate() {
                    if got == 0 {
                        let _ = writeln!(s, "    {term:<width$}  not reached");
                        continue;
                    }
                    let n = t.stopped.get(i).copied().unwrap_or(0);
                    let _ = writeln!(
                        s,
                        "    {term:<width$}  stopped {n} of {got} ({}){}",
                        pct(n, got),
                        seen(t.stopped_at.get(i).copied().flatten())
                    );
                    got = got.saturating_sub(n);
                }
            }
            shown.push(key);
        }
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use sim_core::rules::{CHICKEN, FOX};

    fn world(seed: u64) -> World {
        let cfg = sim_core::Scenario {
            width: 128,
            height: 128,
            seed,
            ..sim_core::Scenario::builtin()
        };
        sim::new_world_with(&cfg, Kinds::builtin()).unwrap()
    }

    #[test]
    fn needs_stats_are_order_statistics() {
        let mut v: Vec<i32> = (1..=101).rev().collect();
        let st = stats(&mut v);
        assert_eq!(
            (st.min, st.p10, st.p50, st.p90, st.max),
            (1, 11, 51, 91, 101)
        );
        assert!((st.mean - 51.0).abs() < 1e-9);
        let st = stats(&mut [7]);
        assert_eq!((st.min, st.p50, st.max), (7, 7, 7));
    }

    #[test]
    fn a_report_samples_at_the_start_every_step_and_the_end() {
        let mut w = world(3);
        let start = sim::tick(&w);
        let opts = Options {
            every: 100,
            sample: 20,
        };
        let r = observe(&mut w, 250, opts);
        let ticks: Vec<u64> = r.samples.iter().map(|s| s.tick - start).collect();
        assert_eq!(ticks, [0, 100, 200, 250]);
        let kinds = w.resource::<Kinds>();
        let first = &r.samples[0].kinds[usize::from(CHICKEN)];
        assert!(first.alive > 0, "the built-in world starts chickens");
        assert_eq!(
            first.events,
            [0; EVENTS.len()],
            "nothing happened before the start"
        );
        // Chickens are sampled, at most 20 a time, and every sampled think
        // decides one thing.
        for s in &r.samples {
            let ks = &s.kinds[usize::from(CHICKEN)];
            assert!(ks.sampled > 0 && ks.sampled <= 20 + 10, "{}", ks.sampled);
            assert_eq!(ks.decisions.values().sum::<u64>(), ks.sampled);
            assert!(ks.rules.keys().all(|r| r.contains(".rules:")));
            // A rule reached either fired or one of its terms stopped it.
            for t in ks.rules.values() {
                assert_eq!(t.stopped.len(), t.stopped_at.len());
                assert!(t.fired + t.stopped.iter().sum::<u64>() <= t.reached);
            }
            assert_eq!(ks.needs.len(), kinds.def(CHICKEN).needs.len());
            assert_eq!(ks.results.iter().sum::<u64>(), ks.alive);
        }
        // Thinks since the last sample add up to the run's.
        let thinks: u64 = r
            .samples
            .iter()
            .map(|s| s.kinds[usize::from(CHICKEN)].events[4])
            .sum();
        assert_eq!(thinks, w.resource::<Tally>().get(CHICKEN, life::THINKS));
    }

    #[test]
    fn the_same_world_observes_the_same() {
        let a = observe(
            &mut world(5),
            300,
            Options {
                every: 150,
                sample: 10,
            },
        );
        let b = observe(
            &mut world(5),
            300,
            Options {
                every: 150,
                sample: 10,
            },
        );
        assert_eq!(a, b);
    }

    #[test]
    fn the_csv_and_the_summary_name_what_they_measure() {
        let mut w = world(3);
        let r = observe(
            &mut w,
            200,
            Options {
                every: 100,
                sample: 10,
            },
        );
        let kinds = w.resource::<Kinds>();
        let mut csv = Vec::new();
        write_csv(&r, kinds, &mut csv).unwrap();
        let csv = String::from_utf8(csv).unwrap();
        let mut lines = csv.lines();
        assert_eq!(lines.next(), Some("tick,day,hour,kind,metric,value"));
        assert!(lines.all(|l| l.split(',').count() == 6 || l.contains('"')));
        assert!(csv.contains(",chicken,alive,"), "{csv}");
        assert!(csv.contains(",chicken,need.food.p50,"));
        assert!(csv.contains(",fox,sampled,"));
        assert!(csv.contains(",chicken,rule.") && csv.contains(".rules:"));
        let text = summary(&r, kinds, &[]);
        assert!(text.starts_with("alive"), "{text}");
        assert!(text.contains("\nchicken\n  needs    "), "{text}");
        assert!(text.contains("  decides  "), "{text}");
        assert!(text.contains("  fires"), "{text}");
        let only = summary(&r, kinds, &["fox".into()]);
        assert!(
            only.contains("\nfox\n") && !only.contains("\nchicken\n"),
            "{only}"
        );
    }

    /// A rule whose first term is false until a counter passes 5, and its
    /// second after: the explained thinks stop at the first, then at the
    /// second, and never fire it; `rule_report` says so per term, with
    /// where a blob stood.
    #[test]
    fn each_rule_counts_the_term_that_stopped_it() {
        let kinds = sim_core::rules::compile_files(&[(
            "b.rules",
            "kind blob { glyph \"b\"  cadence 1  mem m
               when m > 5 and m < 3 => idle
               when true => m += 1 }",
        )])
        .unwrap();
        let s = sim_core::Scenario::parse(
            "t",
            "size 16 16
             terrain water_level 0 rock_on_soil 0 rock_on_water 0
             start blob at (8, 8)",
        )
        .unwrap();
        let mut w = sim::new_world_with(&s, kinds).unwrap();
        let r = observe(
            &mut w,
            20,
            Options {
                every: 2,
                sample: 10,
            },
        );
        let key = "b.rules:2";
        let mut sum = RuleTally::default();
        for sm in &r.samples {
            if let Some(t) = sm.kinds[0].rules.get(key) {
                sum.reached += t.reached;
                sum.fired += t.fired;
                sum.stopped.resize(t.stopped.len(), 0);
                for (a, b) in sum.stopped.iter_mut().zip(&t.stopped) {
                    *a += b;
                }
            }
        }
        assert_eq!(sum.fired, 0);
        assert_eq!(sum.stopped.len(), 2);
        assert!(sum.stopped[0] > 0 && sum.stopped[1] > 0, "{sum:?}");
        assert_eq!(sum.stopped.iter().sum::<u64>(), sum.reached);
        let text = rule_report(&r, w.resource::<Kinds>(), &[]);
        assert!(
            text.starts_with("blob rules, over 11 explained thinks"),
            "{text}"
        );
        assert!(
            text.contains("  b.rules:2  when m > 5 and m < 3 => idle\n"),
            "{text}"
        );
        assert!(text.contains("    m > 5  stopped "), "{text}");
        assert!(text.contains("e.g. (8, 8) after 0 ticks"), "{text}");
        assert!(text.contains("    m < 3  stopped "), "{text}");
    }

    /// The fox in a pen eats the hen: the summary counts the kill as the
    /// fox's, a day's rate and per head, and the hen's death per head.
    #[test]
    fn kills_are_counted_per_killer_and_per_head() {
        let s = sim_core::Scenario::parse(
            "pen",
            "seed 3
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
             }",
        )
        .unwrap();
        let mut w = sim::new_world_with(&s, Kinds::builtin()).unwrap();
        let r = observe(&mut w, TICKS_PER_HOUR, Options::default());
        let text = summary(&r, w.resource::<Kinds>(), &[]);
        assert!(
            text.contains("\nkills a day       all  per head  of\n"),
            "{text}"
        );
        // One kill in an hour, by one fox: 24 a day, 24 per head.
        assert!(
            text.contains("\nfox              24.0     24.00  chicken 24.0\n"),
            "{text}"
        );
        assert!(text.contains("per head  causes of death"), "{text}");
    }

    #[test]
    fn shares_round_to_whole_percents() {
        assert_eq!(pct(1, 3), "33%");
        assert_eq!(pct(2, 3), "67%");
        assert_eq!(pct(1, 1000), "<1%");
        assert_eq!(pct(0, 1000), "0%");
        assert_eq!(pct(0, 0), "0%");
    }

    #[test]
    fn causes_read_as_people_say_them() {
        let kinds = Kinds::builtin();
        let food = kinds
            .def(CHICKEN)
            .needs
            .iter()
            .position(|n| n.name == "food")
            .unwrap();
        let food = u8::try_from(food).unwrap();
        assert_eq!(
            cause_name(&kinds, CHICKEN, Cause::Killed(FOX)),
            "eaten by fox"
        );
        assert_eq!(cause_name(&kinds, CHICKEN, Cause::Starved(food)), "no food");
        assert_eq!(cause_name(&kinds, CHICKEN, Cause::Died), "own die");
        assert_eq!(cause_key(&kinds, CHICKEN, Cause::Killed(FOX)), "killed.fox");
        assert_eq!(
            cause_key(&kinds, CHICKEN, Cause::Starved(food)),
            "starved.food"
        );
        assert_eq!(field("a,b"), "\"a,b\"");
        assert_eq!(field("say \"hi\""), "\"say \"\"hi\"\"\"");
        assert_eq!(field("x\"y,"), "\"x\"\"y,\"");
    }
}
