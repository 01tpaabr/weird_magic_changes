//! The simulation fuzzer: generated rules programs (`rules/gen_rules.rs`,
//! lively mode) run in small generated worlds, and after every tick the
//! world keeps its invariants ([`super::invariants`], [`super::unique_uids`])
//! and nothing has panicked (a trap is the program's business; a panic is a
//! bug). And the same case gives the same checksum after every tick on
//! another thread count (a child process: the compute pool is one per
//! process), after a save and a reopen, and after a reload of the same
//! rules.
//!
//! A case is two texts, a rules file and a scenario, so a failure shrinks
//! to files that go under `scenarios/tests/` as a regression test. `make
//! test` runs a few cases at a fixed seed; `make fuzz` many from a new one
//! (`WMC_FUZZ_CASES`, `WMC_FUZZ_SEED`, see `crate::fuzz_config`).

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};

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
    /// A kind and the `d` of its `start K 1 / d`; one past a whole is
    /// left out.
    shares: Vec<(Index, u32)>,
    /// A kind and a cell, and maybe a need or mem it starts with (which,
    /// an edge or any value, the value); one on water, rock or a taken cell
    /// is left out.
    ats: Vec<(Index, i32, i32, Option<(Index, u8, i32)>)>,
    ticks: u64,
}

fn knobs() -> impl Strategy<Value = Knobs> {
    (
        0u64..1_000_000,
        // 1 x 1 to 2 x 2 chunks, the last row and column mostly whole.
        (1u32..=2, 1u32..=2, 0u32..32, 0u32..32),
        (1u32..=40, 0u32..=60, 0u32..=20, 0u32..=20),
        // Now and then a crowd: every other cell, or every one.
        prop::collection::vec(
            (any::<Index>(), prop_oneof![9 => 8u32..=64, 1 => 1u32..=2]),
            1..=3,
        ),
        prop::collection::vec(
            (
                any::<Index>(),
                0i32..128,
                0i32..128,
                prop::option::of((any::<Index>(), 0u8..3, any::<i32>())),
            ),
            0..=4,
        ),
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
    let mut whole = 0;
    for (i, d) in &k.shares {
        if p.kinds.is_empty() {
            break;
        }
        let kind = i.get(&p.kinds).as_str();
        let share = crate::scenario::PLACE_ONE / d;
        if !used.contains(&kind) && whole + share <= crate::scenario::PLACE_ONE {
            used.push(kind);
            whole += share;
            text += &format!("start {kind} 1 / {d}\n");
        }
    }
    // Only walkable cells, one start each: the terrain is the seed's.
    let s = Scenario::parse("fuzz.scenario", &text).expect("a generated scenario parses");
    let kinds = crate::rules::compile("fuzz.rules", &p.text).expect("a valid program compiles");
    let mut cells: Vec<(i32, i32)> = Vec::new();
    for (i, x, y, with) in &k.ats {
        if p.kinds.is_empty() || cells.contains(&(*x, *y)) {
            continue;
        }
        let (g, f) = s.terrain().cell(*x, *y);
        if !g.walkable() || f.blocks() {
            continue;
        }
        cells.push((*x, *y));
        let def = kinds.by_name(i.get(&p.kinds)).expect("a kind it declares");
        text += &format!("start {} at ({x}, {y})", def.name);
        // A need at 0, full, or anywhere between; a mem at either end of
        // what a scenario can write, or anything.
        let names: Vec<(&str, Option<i32>)> = def
            .needs
            .iter()
            .map(|n| (n.name.as_str(), Some(n.max)))
            .chain(def.mems.iter().map(|m| (m.as_str(), None)))
            .collect();
        if let (Some((slot, edge, v)), false) = (with, names.is_empty()) {
            let (name, max) = *slot.get(&names);
            let v = match (max, edge) {
                (Some(_), 0) => 0,
                (Some(max), 1) => max,
                (Some(max), _) => v.rem_euclid(max.saturating_add(1).max(1)),
                (None, 0) => -i32::MAX,
                (None, 1) => i32::MAX,
                (None, _) => (*v).max(-i32::MAX),
            };
            text += &format!(" with ({name} = {v})");
        }
        text += "\n";
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

/// Where a case's files go: its rules, its scenario, its save. Removed
/// when dropped, after the child that reads them is stopped.
struct Scratch {
    dir: PathBuf,
    child: Option<Child>,
}

impl Drop for Scratch {
    fn drop(&mut self) {
        if let Some(mut c) = self.child.take() {
            let _ = c.kill();
            let _ = c.wait();
        }
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// The variable that hands a case to [`checksums_for_the_parent`].
const CASE_DIR: &str = "WMC_SIMFUZZ_CASE";

/// Start this test binary again, running only
/// [`checksums_for_the_parent`] on `threads` threads: the compute pool is
/// one per process, so another thread count is another process.
fn spawn_child(dir: &Path, threads: usize) -> Result<Child, String> {
    let exe = std::env::current_exe().map_err(|e| format!("the test binary: {e}"))?;
    let path = module_path!().split_once("::").expect("in a crate").1;
    Command::new(exe)
        .args([
            &format!("{path}::checksums_for_the_parent"),
            "--exact",
            "--ignored",
            "--nocapture",
            "--test-threads",
            "1",
        ])
        .env(CASE_DIR, dir)
        .env("WMC_THREADS", threads.to_string())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("starting the child: {e}"))
}

/// Run the case, checking the invariants after every tick, and compare
/// its checksum after every tick with (a) the same case on another thread
/// count, and, from tick `split` on, (b) a world saved at `split` and
/// reopened, which (c) reloads the same rules halfway through the rest.
fn run(c: &Case, split: u64) -> Result<(), String> {
    static CASES: AtomicU64 = AtomicU64::new(0);
    let n = CASES.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("wmc-simfuzz-{}-{n}", std::process::id()));
    let mut scratch = Scratch {
        dir: dir.clone(),
        child: None,
    };
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    std::fs::write(dir.join("fuzz.rules"), &c.rules).map_err(|e| e.to_string())?;
    std::fs::write(dir.join("fuzz.scenario"), &c.scenario).map_err(|e| e.to_string())?;
    let threads = crate::par::init_task_pool();
    let other = if threads == 1 { 8 } else { 1 };
    scratch.child = Some(spawn_child(&dir, other)?);

    // The run, with a save at `split`.
    let (mut w, ticks) = world_of(c);
    let store = Store::open(dir.join("save")).map_err(|e| e.to_string())?;
    invariants(&mut w).map_err(|e| format!("at the start: {e}"))?;
    unique_uids(&mut w).map_err(|e| format!("at the start: {e}"))?;
    let mut sums = Vec::with_capacity(ticks as usize);
    for t in 1..=ticks {
        step(&mut w);
        let at = |e| format!("after tick {t}: {e}");
        invariants(&mut w).map_err(at)?;
        unique_uids(&mut w).map_err(at)?;
        sums.push(checksum(&mut w));
        if t == split {
            save(&mut w, &store).map_err(|e| format!("saving at tick {t}: {e}"))?;
        }
    }
    let sum = |t: u64| sums[t as usize - 1];

    // (b) Reopened at `split`, the same chunks loaded; (c) the same rules
    // reloaded halfway to the end.
    let kinds = || crate::rules::compile("fuzz.rules", &c.rules).expect("it compiled before");
    let mut back = open_world_with(&store, kinds())
        .map_err(|e| format!("reopening the save of tick {split}: {e}"))?
        .ok_or("the save holds no world")?;
    let everywhere = LoadPolicy {
        load: 0,
        unload: 64,
    };
    for cc in w.resource::<Stage>().loaded_coords() {
        ensure_loaded(&mut back, cc.cell(0), everywhere, Some(&store))
            .map_err(|e| format!("loading {cc:?} of the save of tick {split}: {e}"))?;
    }
    if checksum(&mut back) != sum(split) {
        return Err(format!("saved at tick {split} and reopened: another world"));
    }
    let reload_at = split + (ticks - split) / 2;
    for t in split + 1..=ticks {
        step(&mut back);
        if checksum(&mut back) != sum(t) {
            let reloaded = if t > reload_at {
                format!(", the rules reloaded after tick {reload_at}")
            } else {
                String::new()
            };
            return Err(format!(
                "saved at tick {split}, reopened{reloaded}: another world after tick {t}"
            ));
        }
        if t == reload_at {
            crate::reload::reload_rules(&mut back, Some(&store), kinds())
                .map_err(|e| format!("reloading the same rules after tick {t}: {e}"))?;
            if checksum(&mut back) != sum(t) {
                return Err(format!(
                    "reloading the same rules after tick {t} changed the world"
                ));
            }
        }
    }

    // (a) Another thread count.
    let out = scratch
        .child
        .take()
        .expect("started")
        .wait_with_output()
        .map_err(|e| format!("the child: {e}"))?;
    let stdout = String::from_utf8_lossy(&out.stdout);
    if !out.status.success() {
        return Err(format!(
            "on {other} threads, the child failed:\n{stdout}{}",
            String::from_utf8_lossy(&out.stderr)
        ));
    }
    // libtest writes its `test NAME ... ` before the first line.
    let mut lines = stdout
        .lines()
        .filter_map(|l| l.split_once("simfuzz ").map(|(_, l)| l));
    if lines.next() != Some(&format!("threads {other}")) {
        return Err(format!(
            "the child did not run on {other} threads:\n{stdout}"
        ));
    }
    let theirs: Vec<u64> = lines
        .map(|l| {
            let (_, hex) = l.split_once(' ').unwrap_or_default();
            u64::from_str_radix(hex, 16).unwrap_or_default()
        })
        .collect();
    if theirs.len() != sums.len() {
        return Err(format!("the child ran {} ticks of {ticks}", theirs.len()));
    }
    if let Some(t) = (0..sums.len()).find(|&i| theirs[i] != sums[i]) {
        return Err(format!(
            "on {threads} threads and on {other}, another world after tick {}",
            t + 1
        ));
    }
    Ok(())
}

/// The other thread count of [`run`], in a child process: the case in
/// `WMC_SIMFUZZ_CASE`'s directory, its checksum after every tick.
/// Nothing without it.
#[test]
#[ignore = "a child process of generated_worlds_keep_their_invariants_and_agree runs it"]
fn checksums_for_the_parent() {
    let Some(dir) = std::env::var_os(CASE_DIR).map(PathBuf::from) else {
        return;
    };
    let read = |f: &str| std::fs::read_to_string(dir.join(f)).expect("the case's files");
    let c = Case {
        rules: read("fuzz.rules"),
        scenario: read("fuzz.scenario"),
    };
    let (mut w, ticks) = world_of(&c);
    let mut out = String::new();
    out += &format!("simfuzz threads {}\n", crate::par::thread_count());
    for t in 1..=ticks {
        step(&mut w);
        out += &format!("simfuzz {t} {:x}\n", checksum(&mut w));
    }
    print!("{out}");
}

/// [`run`], with a panic turned into a failure that shows the case.
fn check(c: &Case, split: u64) -> Result<(), TestCaseError> {
    let r = catch_unwind(AssertUnwindSafe(|| run(c, split))).unwrap_or_else(|p| {
        let msg = p
            .downcast_ref::<String>()
            .cloned()
            .or_else(|| p.downcast_ref::<&str>().map(|s| s.to_string()))
            .unwrap_or_default();
        Err(format!("panicked: {msg}"))
    });
    r.map_err(|e| TestCaseError::fail(format!("{e}\n{c}(saved after tick {split})")))
}

proptest! {
    // A case takes ~0.2 s: shrinking one takes minutes.
    #![proptest_config(proptest::test_runner::Config {
        max_shrink_iters: 1024,
        ..crate::fuzz_config(16)
    })]

    /// A generated program in a generated world keeps every invariant,
    /// tick after tick, never panics, and runs to the same world on
    /// another thread count, through a save and a reopen, and a reload.
    #[test]
    fn generated_worlds_keep_their_invariants_and_agree(
        p in gen_rules::program(Mode::Lively),
        k in knobs(),
        split in 1u64..100,
    ) {
        check(&case(&p, &k), (k.ticks * split / 100).max(1))?;
    }
}
