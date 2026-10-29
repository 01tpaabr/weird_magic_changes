//! `wmc observe`: how a world's populations move over time, and why. The
//! middle zoom level between `wmc why` (one actor's next think) and `wmc
//! run` (totals at the end), for tuning a world's dynamics.
//!
//! Every `every` ticks it samples each kind: how many are alive; births and
//! deaths since the last sample, deaths by cause (eaten by which kind,
//! starved of which need, or a rule's own `die`); its needs over the alive
//! (min, p10, median, p90, max, mean, decayed to now); how many are in each
//! state; the last results of their actions; and, for up to `sample` of
//! them, what their next think would decide and which rules would fire (a
//! dry run, as the inspector does: nothing in the world changes).
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
    /// Rules that fired in them, by `file:line`.
    pub rules: BTreeMap<String, u64>,
}

/// Every kind at one tick.
#[derive(Debug, Clone, PartialEq)]
pub struct Sample {
    pub tick: u64,
    pub kinds: Vec<KindSample>,
}

/// A whole observation: the samples, from the first tick to the last, and
/// the text of every rule they name.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Report {
    pub samples: Vec<Sample>,
    pub rule_text: BTreeMap<String, String>,
}

/// Step `world` for `ticks`, sampling at the start, every `opts.every`
/// ticks, and at the end.
pub fn observe(world: &mut World, ticks: u64, opts: Options) -> Report {
    let every = opts.every.max(1);
    let mut report = Report::default();
    let mut seen = world.resource::<Tally>().clone();
    report
        .samples
        .push(sample(world, opts, &seen, &mut report.rule_text));
    let mut done = 0;
    while done < ticks {
        let n = every.min(ticks - done);
        for _ in 0..n {
            sim::step(world);
        }
        done += n;
        report
            .samples
            .push(sample(world, opts, &seen, &mut report.rule_text));
        seen = world.resource::<Tally>().clone();
    }
    report
}

/// One sample of `world`, with life events counted since `seen`.
fn sample(
    world: &World,
    opts: Options,
    seen: &Tally,
    rule_text: &mut BTreeMap<String, String>,
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
            for c in checked_rules(kinds, &e)
                .iter()
                .filter(|c| c.verdict == "FIRED")
            {
                let key = format!("{}:{}", c.file, c.rule.line);
                rule_text
                    .entry(key.clone())
                    .or_insert_with(|| c.rule.text.clone());
                *ks.rules.entry(key).or_default() += 1;
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
fn field(s: &str) -> String {
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
            for (r, n) in &ks.rules {
                row(&format!("rule.{r}"), n.to_string())?;
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

    // Births and deaths per day, deaths by cause.
    let _ = writeln!(
        s,
        "\nper day          born  became    died  causes of death"
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
        let _ = writeln!(
            s,
            "{name:<14}{:>7} {:>7} {:>7}  {}",
            rate(born),
            rate(became),
            rate(dead),
            why.join(", ")
        );
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
                for (r, &n) in &ks.rules {
                    *rules.entry(r.as_str()).or_default() += n;
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
                let text = report.rule_text.get(*r).map_or(String::new(), |t| {
                    t.split_whitespace().collect::<Vec<_>>().join(" ")
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
