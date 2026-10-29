//! `wmc lab`: a scenario test (a goal, RULES §14) across values of the
//! rules' constants and across seeds, many runs at once. For each variant
//! (one combination of the `--set NAME=a,b,...` values): on how many seeds
//! every expectation held, and per expectation, on how many seeds it held
//! and the spread of its key number across them (the lowest count an
//! `always >=` saw, a mean, a rate: `sim::Event::Expected`).
//!
//! Each run is one world on one thread; `jobs` of them run at once, taking
//! the next run from a shared counter. Results come back by index, so the
//! report is the same however many jobs ran it, and on any thread count. A
//! seed stops at its first sure failure (`sim::run_checks_until`) unless
//! `full`: a variant that fails early costs little.

use std::fmt::Write as _;
use std::io::{self, Write};
use std::panic::AssertUnwindSafe;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc;

use sim_core::rules::ConstSet;
use sim_core::{Kinds, Scenario, sim};

use crate::observe::field;

/// One combination of constant values, and the rules compiled with it.
#[derive(Debug, Clone)]
pub struct Variant {
    /// `NAME=a OTHER=x`, as given; empty for the rules as written.
    pub label: String,
    pub kinds: Kinds,
}

/// An expectation's verdict on one run.
#[derive(Debug, Clone, PartialEq)]
pub struct Verdict {
    pub line: u32,
    pub text: String,
    pub held: bool,
    /// Its key number, if it has one.
    pub value: Option<f64>,
    /// What it saw, or why it was not checked.
    pub saw: String,
}

impl Verdict {
    /// Was it checked, or did its run stop before it?
    pub fn reached(&self) -> bool {
        !self.saw.starts_with("not reached")
    }
}

/// One variant on one seed.
#[derive(Debug, Clone, PartialEq)]
pub struct Run {
    pub seed: u64,
    pub verdicts: Vec<Verdict>,
    /// It stopped at its first sure failure.
    pub stopped: bool,
    /// Its world could not be made, or it panicked.
    pub error: Option<String>,
}

impl Run {
    /// Did every expectation hold?
    pub fn held(&self) -> bool {
        self.error.is_none() && self.verdicts.iter().all(|v| v.held)
    }
}

/// Every combination of the values `--set NAME=a,b,c` lists, the first
/// name varying slowest, each as its label and its constants. `sets`: per
/// name, its values as written and as numbers. No sets: one combination,
/// the rules as written.
pub fn variants(sets: &[(String, Vec<(String, i32)>)]) -> Vec<(String, Vec<ConstSet>)> {
    let mut out = vec![(String::new(), Vec::new())];
    for (name, values) in sets {
        out = out
            .into_iter()
            .flat_map(|(label, consts)| {
                values.iter().map(move |(text, v)| {
                    let label = if label.is_empty() {
                        format!("{name}={text}")
                    } else {
                        format!("{label} {name}={text}")
                    };
                    let mut consts = consts.clone();
                    consts.push((name.clone(), *v));
                    (label, consts)
                })
            })
            .collect();
    }
    out
}

/// Every variant on `seeds` seeds (`scenario.seed`, `+ 1`, ...), `jobs`
/// at a time; `progress` hears each run end (done, total). Results in
/// variant order, each variant's in seed order.
pub fn run(
    scenario: &Scenario,
    variants: &[Variant],
    seeds: u32,
    jobs: usize,
    full: bool,
    progress: impl Fn(usize, usize) + Sync,
) -> Vec<Vec<Run>> {
    let per = seeds as usize;
    let total = variants.len() * per;
    let (next, done) = (AtomicUsize::new(0), AtomicUsize::new(0));
    let (tx, rx) = mpsc::channel();
    std::thread::scope(|s| {
        for _ in 0..jobs.clamp(1, total.max(1)) {
            let tx = tx.clone();
            let (next, done, progress) = (&next, &done, &progress);
            s.spawn(move || {
                loop {
                    let i = next.fetch_add(1, Ordering::Relaxed);
                    if i >= total {
                        break;
                    }
                    let (v, k) = (i / per, i % per);
                    let seed = scenario.seed.wrapping_add(k as u64);
                    let _ = tx.send((i, one(scenario, &variants[v].kinds, seed, full)));
                    progress(done.fetch_add(1, Ordering::Relaxed) + 1, total);
                }
            });
        }
    });
    drop(tx);
    let mut runs: Vec<Option<Run>> = vec![None; total];
    for (i, r) in rx {
        runs[i] = Some(r);
    }
    let mut runs = runs.into_iter().map(|r| r.expect("every run reports"));
    (0..variants.len())
        .map(|_| runs.by_ref().take(per).collect())
        .collect()
}

/// One run: the scenario on `seed` under `kinds`, its checks stopping at
/// the first sure failure unless `full`.
fn one(scenario: &Scenario, kinds: &Kinds, seed: u64, full: bool) -> Run {
    let s = Scenario {
        seed,
        ..scenario.clone()
    };
    let mut verdicts = Vec::new();
    let work = std::panic::catch_unwind(AssertUnwindSafe(|| {
        let mut world = sim::new_world_with(&s, kinds.clone()).map_err(|e| e.to_string())?;
        Ok::<bool, String>(sim::run_checks_until(&mut world, &s.checks, !full, |e| {
            if let sim::Event::Expected {
                line,
                text,
                result,
                value,
            } = e
            {
                let (held, saw) = match result {
                    Ok((held, saw)) => (held, saw),
                    Err(e) => (false, e),
                };
                verdicts.push(Verdict {
                    line,
                    text: text.to_string(),
                    held,
                    value,
                    saw,
                });
            }
        }))
    }));
    let (stopped, error) = match work {
        Ok(Ok(stopped)) => (stopped, None),
        Ok(Err(e)) => (false, Some(e)),
        Err(_) => (false, Some("it panicked (stderr says where)".to_string())),
    };
    Run {
        seed,
        verdicts,
        stopped,
        error,
    }
}

/// `x` to two decimals, without trailing zeros: `0.03`, `24`, `2.5`.
fn num(x: f64) -> String {
    let s = format!("{x:.2}");
    s.trim_end_matches('0').trim_end_matches('.').to_string()
}

/// `   (214 .. 260, median 231)`, or `   (24)` when they agree; nothing for
/// no numbers.
fn spread(v: &mut [f64]) -> String {
    if v.is_empty() {
        return String::new();
    }
    v.sort_by(f64::total_cmp);
    let (lo, hi, mid) = (v[0], v[v.len() - 1], v[v.len() / 2]);
    if lo == hi {
        format!("   ({})", num(lo))
    } else {
        format!("   ({} .. {}, median {})", num(lo), num(hi), num(mid))
    }
}

/// The report: per variant, its verdict (a variant passes when at least
/// `pass` seeds held every expectation), then per expectation the seeds it
/// held on and the spread of its number; last, which variants passed.
pub fn report(file: &str, variants: &[Variant], runs: &[Vec<Run>], pass: u32) -> String {
    let mut s = String::new();
    let label = |v: &Variant| {
        if v.label.is_empty() {
            "the rules as written".to_string()
        } else {
            v.label.clone()
        }
    };
    let width = variants.iter().map(|v| label(v).len()).max().unwrap_or(0);
    let mut passed = Vec::new();
    for (i, (v, rs)) in variants.iter().zip(runs).enumerate() {
        let held = rs.iter().filter(|r| r.held()).count();
        let ok = held >= pass as usize;
        let stopped = rs.iter().filter(|r| r.stopped).count();
        let _ = writeln!(
            s,
            "variant {:<3} {:<width$}  {}  {held} of {} seeds held every expectation, {pass} needed{}",
            i + 1,
            label(v),
            if ok { "ok  " } else { "FAIL" },
            rs.len(),
            if stopped > 0 {
                format!("; {stopped} stopped early")
            } else {
                String::new()
            }
        );
        if ok {
            passed.push(format!("{} ({})", i + 1, label(v)));
        }
        let mut errors: Vec<&str> = rs.iter().filter_map(|r| r.error.as_deref()).collect();
        errors.dedup();
        for e in errors {
            let _ = writeln!(s, "             error: {e}");
        }
        let lines = rs.iter().map(|r| r.verdicts.len()).max().unwrap_or(0);
        for k in 0..lines {
            let vs: Vec<&Verdict> = rs.iter().filter_map(|r| r.verdicts.get(k)).collect();
            let Some(first) = vs.first() else { continue };
            let held = vs.iter().filter(|v| v.held).count();
            let unreached = vs.iter().filter(|v| !v.reached()).count();
            let mut nums: Vec<f64> = vs.iter().filter_map(|v| v.value).collect();
            let _ = writeln!(
                s,
                "  {held:>4}/{:<4} {file}:{}: {}{}{}",
                rs.len(),
                first.line,
                first.text,
                spread(&mut nums),
                if unreached > 0 {
                    format!("   [{unreached} not reached]")
                } else {
                    String::new()
                }
            );
        }
    }
    let _ = writeln!(
        s,
        "\npassed    {}",
        if passed.is_empty() {
            "none".to_string()
        } else {
            passed.join(", ")
        }
    );
    s
}

/// Every verdict as a CSV row:
/// `variant,consts,seed,line,expectation,held,value,saw`.
pub fn write_csv(variants: &[Variant], runs: &[Vec<Run>], out: &mut impl Write) -> io::Result<()> {
    writeln!(out, "variant,consts,seed,line,expectation,held,value,saw")?;
    for (i, (v, rs)) in variants.iter().zip(runs).enumerate() {
        let consts = field(&v.label);
        for r in rs {
            if let Some(e) = &r.error {
                writeln!(out, "{},{consts},{},,,false,,{}", i + 1, r.seed, field(e))?;
            }
            for x in &r.verdicts {
                writeln!(
                    out,
                    "{},{consts},{},{},{},{},{},{}",
                    i + 1,
                    r.seed,
                    x.line,
                    field(&x.text),
                    x.held,
                    x.value.map_or(String::new(), num),
                    field(&x.saw)
                )?;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Blobs that bud with chance `BUD` in a hundred at each think: with
    /// `BUD = 0` there is only the first, with more they fill the pen.
    const RULES: &str = "const BUD = 50
        kind blob { glyph \"b\"  cadence 16
          when chance(BUD) and nearest free within 1 as c => spawn blob at c }";

    const GOAL: &str = "seed 7
        seeds 3
        size 16 16
        terrain water_level 0 rock_on_soil 0 rock_on_water 0
        start blob at (8, 8)
        run 2h
        expect always count blob <= 1
        expect mean born blob per day >= 1";

    fn lab(
        sets: &[(String, Vec<(String, i32)>)],
        jobs: usize,
        full: bool,
    ) -> (Vec<Variant>, Vec<Vec<Run>>) {
        let scenario = Scenario::parse("goal", GOAL).unwrap();
        let variants: Vec<Variant> = variants(sets)
            .into_iter()
            .map(|(label, consts)| Variant {
                label,
                kinds: sim_core::rules::compile_files_with(&[("b.rules", RULES)], &consts).unwrap(),
            })
            .collect();
        sim_core::par::init_task_pool();
        let runs = run(&scenario, &variants, scenario.seeds, jobs, full, |_, _| {});
        (variants, runs)
    }

    fn bud(values: &[i32]) -> Vec<(String, Vec<(String, i32)>)> {
        vec![(
            "BUD".into(),
            values.iter().map(|v| (v.to_string(), *v)).collect(),
        )]
    }

    #[test]
    fn variants_are_every_combination_first_name_slowest() {
        let sets = vec![
            ("A".to_string(), vec![("1".into(), 1), ("2h".into(), 1800)]),
            (
                "B".to_string(),
                vec![("x".into(), -3), ("y".into(), 4), ("z".into(), 5)],
            ),
        ];
        let v = variants(&sets);
        let labels: Vec<&str> = v.iter().map(|(l, _)| l.as_str()).collect();
        assert_eq!(
            labels,
            [
                "A=1 B=x", "A=1 B=y", "A=1 B=z", "A=2h B=x", "A=2h B=y", "A=2h B=z"
            ]
        );
        assert_eq!(v[3].1, [("A".to_string(), 1800), ("B".to_string(), -3)]);
        assert_eq!(variants(&[]), [(String::new(), Vec::new())]);
    }

    #[test]
    fn each_variant_is_judged_on_its_seeds() {
        let (variants, runs) = lab(&bud(&[0, 50]), 4, false);
        assert_eq!(runs.len(), 2);
        assert_eq!(
            runs[0].iter().map(|r| r.seed).collect::<Vec<_>>(),
            [7, 8, 9]
        );
        // No budding: the blob stays alone, and nothing is born.
        assert!(runs[0].iter().all(|r| !r.held() && !r.stopped));
        assert!(
            runs[0]
                .iter()
                .all(|r| r.verdicts[0].held && !r.verdicts[1].held)
        );
        // Budding: `always <= 1` fails, and the seed stops there.
        assert!(
            runs[1]
                .iter()
                .all(|r| r.stopped && !r.verdicts[0].held && !r.verdicts[1].reached())
        );
        let text = report("goal", &variants, &runs, 3);
        assert!(
            text.contains(
                "variant 1   BUD=0   FAIL  0 of 3 seeds held every expectation, 3 needed"
            ),
            "{text}"
        );
        assert!(
            text.contains("   3/3    goal:7: expect always count blob <= 1   (1)"),
            "{text}"
        );
        assert!(
            text.contains("   0/3    goal:8: expect mean born blob per day >= 1   (0)"),
            "{text}"
        );
        assert!(
            text.contains("; 3 stopped early") && text.contains("[3 not reached]"),
            "{text}"
        );
        assert!(text.ends_with("passed    none\n"), "{text}");
    }

    #[test]
    fn the_report_is_the_same_however_many_jobs_run_it() {
        let one = lab(&bud(&[0, 20, 50]), 1, true);
        let four = lab(&bud(&[0, 20, 50]), 4, true);
        assert_eq!(one.1, four.1);
        assert_eq!(
            report("g", &one.0, &one.1, 2),
            report("g", &four.0, &four.1, 2)
        );
        // Run in full, a budding variant reads its rate on every seed.
        let rates: Vec<f64> = one.1[2]
            .iter()
            .filter_map(|r| r.verdicts[1].value)
            .collect();
        assert_eq!(rates.len(), 3);
        assert!(rates.iter().all(|&r| r > 1.0), "{rates:?}");
        let mut csv = Vec::new();
        write_csv(&one.0, &one.1, &mut csv).unwrap();
        let csv = String::from_utf8(csv).unwrap();
        assert!(csv.starts_with("variant,consts,seed,line,expectation,held,value,saw\n"));
        assert_eq!(csv.lines().count(), 1 + 3 * 3 * 2, "a row per verdict");
        assert!(
            csv.contains("\n1,BUD=0,7,7,expect always count blob <= 1,true,1,"),
            "{csv}"
        );
    }

    #[test]
    fn spreads_and_numbers_read_short() {
        assert_eq!(spread(&mut []), "");
        assert_eq!(spread(&mut [24.0, 24.0]), "   (24)");
        assert_eq!(spread(&mut [3.0, 1.0, 2.5]), "   (1 .. 3, median 2.5)");
        assert_eq!(num(0.126), "0.13");
    }
}
