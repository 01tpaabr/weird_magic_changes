//! The simulation fuzzer: generated rules programs (`rules/gen_rules.rs`,
//! lively mode) run in small generated worlds, and after every tick the
//! world keeps its invariants ([`super::invariants`], [`super::unique_uids`])
//! and nothing has panicked (a trap is the program's business; a panic is a
//! bug).
//!
//! A case is two texts, a rules file and a scenario, so a failure shrinks
//! to files that go under `scenarios/tests/` as a regression test. `make
//! test` runs a few cases at a fixed seed; `make fuzz` many from a new one
//! (`WMC_FUZZ_CASES`, `WMC_FUZZ_SEED`, see `crate::fuzz_config`).

use std::panic::{AssertUnwindSafe, catch_unwind};

use proptest::prelude::*;
use proptest::sample::Index;
use proptest::test_runner::TestCaseError;

use super::*;
use crate::rules::gen_rules::{self, Mode, Program};

/// The world a program runs in, as numbers: the scenario is a pure
/// function of them and the program's kinds, so both shrink.
#[derive(Debug, Clone)]
struct Knobs {
    seed: u64,
    width: u32,
    height: u32,
    /// `water_scale`, and the rest in hundredths.
    terrain: (u32, u32, u32, u32),
    /// A kind and the `d` of its `start K 1 / d`.
    shares: Vec<(Index, u32)>,
    /// A kind and a cell; one on water, rock or a taken cell is left out.
    ats: Vec<(Index, i32, i32)>,
    ticks: u64,
}

fn knobs() -> impl Strategy<Value = Knobs> {
    (
        0u64..1_000_000,
        // 1 x 1 to 2 x 2 chunks, the last row and column mostly whole.
        (1u32..=2, 1u32..=2, 0u32..32, 0u32..32),
        (1u32..=40, 0u32..=60, 0u32..=20, 0u32..=20),
        prop::collection::vec((any::<Index>(), 8u32..=64), 1..=3),
        prop::collection::vec((any::<Index>(), 0i32..128, 0i32..128), 0..=4),
        200u64..=400,
    )
        .prop_map(
            |(seed, (cx, cy, dx, dy), terrain, shares, ats, ticks)| Knobs {
                seed,
                width: cx * 64 - dx,
                height: cy * 64 - dy,
                terrain,
                shares,
                ats,
                ticks,
            },
        )
}

/// A case: the rules, and a scenario of them that ends in `run TICKS`.
#[derive(Debug, Clone)]
struct Case {
    rules: String,
    scenario: String,
}

impl std::fmt::Display for Case {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "--- fuzz.rules ---\n{}\n--- fuzz.scenario ---\n{}",
            self.rules, self.scenario
        )
    }
}

fn case(p: &Program, k: &Knobs) -> Case {
    let (scale, level, soil, water) = k.terrain;
    let mut text = format!(
        "seed {}\nsize {} {}\nterrain water_scale {scale} water_level 0.{level:02} \
         rock_on_soil 0.{soil:02} rock_on_water 0.{water:02}\n",
        k.seed, k.width, k.height
    );
    let mut used: Vec<&str> = Vec::new();
    for (i, d) in &k.shares {
        if p.kinds.is_empty() {
            break;
        }
        let kind = i.get(&p.kinds).as_str();
        if !used.contains(&kind) {
            used.push(kind);
            text += &format!("start {kind} 1 / {d}\n");
        }
    }
    // Only walkable cells, one start each: the terrain is the seed's.
    let s = Scenario::parse("fuzz.scenario", &text).expect("a generated scenario parses");
    let mut cells: Vec<(i32, i32)> = Vec::new();
    for (i, x, y) in &k.ats {
        if p.kinds.is_empty() || cells.contains(&(*x, *y)) {
            continue;
        }
        let (g, f) = s.terrain().cell(*x, *y);
        if g.walkable() && !f.blocks() {
            cells.push((*x, *y));
            text += &format!("start {} at ({x}, {y})\n", i.get(&p.kinds));
        }
    }
    text += &format!("run {}\n", k.ticks);
    Case {
        rules: p.text.clone(),
        scenario: text,
    }
}

/// The case's world and its `run` ticks. A program valid mode made that
/// does not compile, or a scenario that does not fit it, is a bug in this
/// file or in `gen_rules`.
fn world_of(c: &Case) -> (World, u64) {
    let kinds = crate::rules::compile("fuzz.rules", &c.rules).expect("a valid program compiles");
    let s = Scenario::parse("fuzz.scenario", &c.scenario).expect("the scenario parses");
    let ticks = s
        .checks
        .iter()
        .map(|c| match c {
            crate::scenario::Check::Run(t) => *t,
            _ => 0,
        })
        .sum();
    let w = new_world_with(&s, kinds).expect("the scenario fits the rules");
    (w, ticks)
}

/// Run the case, checking the invariants after every tick.
fn run(c: &Case) -> Result<(), String> {
    let (mut w, ticks) = world_of(c);
    invariants(&mut w).map_err(|e| format!("at the start: {e}"))?;
    unique_uids(&mut w).map_err(|e| format!("at the start: {e}"))?;
    for t in 1..=ticks {
        step(&mut w);
        let at = |e| format!("after tick {t}: {e}");
        invariants(&mut w).map_err(at)?;
        unique_uids(&mut w).map_err(at)?;
    }
    Ok(())
}

/// [`run`], with a panic turned into a failure that shows the case.
fn check(c: &Case) -> Result<(), TestCaseError> {
    let r = catch_unwind(AssertUnwindSafe(|| run(c))).unwrap_or_else(|p| {
        let msg = p
            .downcast_ref::<String>()
            .cloned()
            .or_else(|| p.downcast_ref::<&str>().map(|s| s.to_string()))
            .unwrap_or_default();
        Err(format!("panicked: {msg}"))
    });
    r.map_err(|e| TestCaseError::fail(format!("{e}\n{c}")))
}

proptest! {
    // A case takes ~0.1 s: shrinking one takes minutes.
    #![proptest_config(proptest::test_runner::Config {
        max_shrink_iters: 1024,
        ..crate::fuzz_config(8)
    })]

    /// A generated program in a generated world keeps every invariant,
    /// tick after tick, and never panics.
    #[test]
    fn generated_worlds_keep_their_invariants(
        p in gen_rules::program(Mode::Lively),
        k in knobs(),
    ) {
        check(&case(&p, &k))?;
    }
}
