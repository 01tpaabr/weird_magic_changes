//! The CLI on the real binary: what it refuses before it builds a world,
//! and what `wmc why` reports.

use std::path::{Path, PathBuf};
use std::process::Command;

/// `wmc args` with no `WMC_RULES`: success, stdout, stderr.
fn wmc(args: &[&str]) -> (bool, String, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_wmc"))
        .env_remove("WMC_RULES")
        .args(args)
        .output()
        .expect("wmc runs");
    let text = |b: &[u8]| String::from_utf8_lossy(b).into_owned();
    (out.status.success(), text(&out.stdout), text(&out.stderr))
}

/// A fresh scratch directory for one test; the test removes it when it
/// passes.
fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("wmc-cli-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// `why` follows the actor it found: grass under a chicken that walks
/// onto it is still the grass, not the chicken.
#[test]
fn why_follows_ground_cover_under_a_walker() {
    let dir = scratch("pen");
    let scenario = dir.join("pen.scenario");
    std::fs::write(
        &scenario,
        "seed 1\noutside rock\nmap {\n  #####\n  #CG.#\n  #####\n}\n\
         legend {\n  . soil\n  # rock\n  C chicken\n  G grass\n}\n",
    )
    .unwrap();
    let save = dir.join("save");
    let (ok, out, err) = wmc(&[
        "why",
        save.to_str().unwrap(),
        "2",
        "1",
        "0",
        "--scenario",
        scenario.to_str().unwrap(),
    ]);
    assert!(ok, "{err}");
    assert!(out.contains("grass at (2, 1)"), "{out}");
    assert!(out.contains("thinks every 256 ticks, due now"), "{out}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// `why` waits as long as the actor's cadence, not just a day.
#[test]
fn why_waits_out_a_cadence_longer_than_a_day() {
    let dir = scratch("slow");
    std::fs::create_dir_all(dir.join("slow")).unwrap();
    std::fs::write(
        dir.join("slow/stone.rules"),
        "kind stone {\n  glyph \"o\"\n  cadence 32768\n  when true => idle\n}\n",
    )
    .unwrap();
    let scenario = dir.join("slow.scenario");
    std::fs::write(
        &scenario,
        "rules slow\nseed 1\nsize 16 16\nstart stone at (3, 3)\n",
    )
    .unwrap();
    let save = dir.join("save");
    let (ok, out, err) = wmc(&[
        "why",
        save.to_str().unwrap(),
        "3",
        "3",
        "5000",
        "--scenario",
        scenario.to_str().unwrap(),
    ]);
    assert!(ok, "{err}");
    assert!(out.contains("stone at (3, 3)"), "{out}");
    assert!(out.contains("thinks every 32768 ticks, due now"), "{out}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// Every file under `dir` with its bytes, in path order.
fn files_under(dir: &Path) -> Vec<(PathBuf, Vec<u8>)> {
    let mut out = Vec::new();
    for e in std::fs::read_dir(dir).unwrap() {
        let p = e.unwrap().path();
        if p.is_dir() {
            out.extend(files_under(&p));
        } else {
            out.push((p.clone(), std::fs::read(&p).unwrap()));
        }
    }
    out.sort();
    out
}

/// `run` and `why` never save, so they leave the disk as they found it: no
/// save directory where there was none, nothing in an empty one, a saved
/// world byte for byte. An empty save directory is an error for every
/// command that takes one (it was the working directory, where `run` made
/// a `chunks`).
#[test]
fn run_and_why_create_nothing_on_disk() {
    use sim_core::{Scenario, Store, sim};
    let dir = scratch("nothing");
    let tiny = "seed 1\nsize 1 1\nstart fox at (0, 0)\n";
    std::fs::write(dir.join("tiny.scenario"), tiny).unwrap();
    std::fs::create_dir(dir.join("empty")).unwrap();
    let store = Store::open(dir.join("saved")).unwrap();
    sim::save(
        &mut sim::new_world(&Scenario::parse("t", tiny).unwrap()),
        &store,
    )
    .unwrap();
    let before = files_under(&dir);
    for save in ["fresh", "empty", "saved"] {
        let cases: [&[&str]; 2] = [
            &["run", save, "3", "--scenario", "tiny.scenario"],
            &["why", save, "0", "0", "0", "--scenario", "tiny.scenario"],
        ];
        for args in cases {
            let (code, out, err) = wmc_in(&dir, &[], args);
            assert_eq!(code, Some(0), "{args:?}\n{out}{err}");
        }
    }
    for args in [
        &["run", "", "1"][..],
        &["why", "", "0", "0"],
        &["why", "-v", "", "0", "0"],
        &["play", ""],
    ] {
        let (code, out, err) = wmc_in(&dir, &[], args);
        assert_eq!(code, Some(1), "{args:?}\n{out}{err}");
        assert!(err.contains("an empty path"), "{args:?}\n{err}");
    }
    assert!(!dir.join("fresh").exists());
    assert_eq!(files_under(&dir), before);
    assert_eq!(std::fs::read_dir(dir.join("empty")).unwrap().count(), 0);
    std::fs::remove_dir_all(&dir).unwrap();
}

/// `[w h]` on the command line get the checks a `size` line gets.
#[test]
fn a_size_from_the_command_line_is_checked() {
    let (ok, _, err) = wmc(&["show", "0", "0", "1"]);
    assert!(!ok && err.contains("at least 1"), "{err}");
    let (ok, _, err) = wmc(&["show", "100000", "100000", "1"]);
    assert!(!ok && err.contains("at most 4096 up front"), "{err}");
    let bees = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../scenarios/tests/bees.scenario");
    let bees = bees.to_str().unwrap();
    let (ok, _, err) = wmc(&["show", "5", "5", "1", "--scenario", bees]);
    assert!(!ok && err.contains("smaller than the map"), "{err}");
    let (ok, out, err) = wmc(&["show", "20", "5", "42"]);
    assert!(ok, "{out}{err}");
}

/// What the command line does not use is an error, not dropped: an unknown
/// flag, an argument past the last one a command reads, `--strict` off
/// `lint`.
#[test]
fn unused_arguments_are_refused() {
    let repo = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let fox_pen = repo.join("scenarios/tests/fox_pen.scenario");
    let fox_pen = fox_pen.to_str().unwrap();
    let dir = std::env::temp_dir().join(format!("wmc-cli-args-{}", std::process::id()));
    let dir = dir.to_str().unwrap();
    let (ok, _, err) = wmc(&["show", "20", "5", "42", "--sed", "7"]);
    assert!(!ok && err.contains("unknown flag --sed"), "{err}");
    // `--seed` is sprite fit's.
    let (ok, _, err) = wmc(&["show", "20", "5", "42", "--seed", "7"]);
    assert!(!ok && err.contains("only for sprite"), "{err}");
    let (ok, _, err) = wmc(&["show", "20", "5", "42", "7"]);
    assert!(!ok && err.contains("unexpected argument \"7\""), "{err}");
    let (ok, _, err) = wmc(&["run", dir, "10", "30", "20", "7", "extra"]);
    assert!(
        !ok && err.contains("unexpected argument \"extra\""),
        "{err}"
    );
    let (ok, _, err) = wmc(&["scenario", fox_pen, fox_pen]);
    assert!(!ok && err.contains("scenario takes one file"), "{err}");
    let (ok, _, err) = wmc(&["run", dir, "1", "--strict"]);
    assert!(!ok && err.contains("--strict is only for lint"), "{err}");
    let (ok, _, err) = wmc(&["run", dir, "1", "--every", "1h"]);
    assert!(!ok && err.contains("only for observe"), "{err}");
    let (ok, _, err) = wmc(&["run", dir, "1", "--seeds", "2"]);
    assert!(!ok && err.contains("only for scenario"), "{err}");
    let (ok, _, err) = wmc(&["observe", "2h", "10", "10", "1", "extra"]);
    assert!(
        !ok && err.contains("unexpected argument \"extra\""),
        "{err}"
    );
    // What they do use still works.
    let (ok, out, err) = wmc(&["scenario", fox_pen, "--threads", "1"]);
    assert!(ok, "{out}{err}");
    let rules = repo.join("rules");
    let (ok, out, err) = wmc(&["lint", rules.to_str().unwrap(), "--strict"]);
    assert!(ok, "{out}{err}");
    // The VM's constant pool, not the `const` lines it was once read as.
    assert!(out.contains(" ops, const pool "), "{out}");
    assert!(!Path::new(dir).exists(), "run never saves");
}

/// `observe` on a pen where a hungry fox eats a chicken: the chicken's
/// death is counted as eaten by the fox, the CSV holds the same, and the
/// rules that fire are named by file and line.
#[test]
fn observe_reports_who_died_of_what() {
    let dir = scratch("observe");
    let repo = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let fox_pen = repo.join("scenarios/tests/fox_pen.scenario");
    let csv = dir.join("pen.csv");
    let (ok, out, err) = wmc(&[
        "observe",
        "2h",
        "--scenario",
        fox_pen.to_str().unwrap(),
        "--every",
        "30min",
        "--csv",
        csv.to_str().unwrap(),
        "--kind",
        "fox",
    ]);
    assert!(ok, "{err}");
    // Eaten or hit to death: a kill.
    assert!(out.contains("killed by fox 100%"), "{out}");
    assert!(
        out.contains("\nfox\n") && !out.contains("\nchicken\n"),
        "{out}"
    );
    assert!(out.contains("animals.rules:"), "{out}");
    let rows = std::fs::read_to_string(&csv).unwrap();
    assert!(
        rows.starts_with("tick,day,hour,kind,metric,value\n"),
        "{rows}"
    );
    assert!(
        rows.contains(",chicken,death.killed.fox,2\n"),
        "two pens, two chickens: {rows}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// `wmc lab` runs a goal for every `--set` value on each seed: a variant
/// per value, judged on its seeds, the same report however many runs go
/// at once. `--set` with one value sets a const for any command; a list is
/// for lab, and a name the rules lack is an error naming the ones they have.
#[test]
fn lab_sweeps_a_const_across_seeds() {
    let dir = scratch("lab");
    std::fs::write(
        dir.join("blob.rules"),
        "const BUD = 50\nkind blob { glyph \"b\"  cadence 16\n  when chance(BUD) and nearest free within 1 as c => spawn blob at c }\n",
    )
    .unwrap();
    let goal = dir.join("goal.scenario");
    std::fs::write(
        &goal,
        "rules blob.rules\nseed 7\nseeds 3\nsize 16 16\nterrain water_level 0 rock_on_soil 0 rock_on_water 0\nstart blob at (8, 8)\nrun 2h\nexpect always count blob <= 1\n",
    )
    .unwrap();
    let goal = goal.to_str().unwrap();
    let csv = dir.join("lab.csv");
    let lab = |jobs: &str| {
        let (ok, out, err) = wmc(&[
            "lab",
            goal,
            "--set",
            "BUD=0,50",
            "--jobs",
            jobs,
            "--csv",
            csv.to_str().unwrap(),
        ]);
        assert!(ok, "{err}");
        out.lines()
            .filter(|l| !l.starts_with("wall") && !l.starts_with("lab "))
            .collect::<Vec<_>>()
            .join("\n")
    };
    let one = lab("1");
    assert_eq!(one, lab("3"), "the same report at 1 and 3 jobs");
    assert!(
        one.contains("variant 1   BUD=0   ok    3 of 3 seeds held every expectation"),
        "{one}"
    );
    // Three of three to pass: one failed seed and BUD=50 gives up.
    assert!(one.contains("variant 2   BUD=50  FAIL  0 of 1 seeds held every expectation, 3 needed; 1 stopped early; 2 not run, it could no longer pass"), "{one}");
    assert!(one.contains("passed    1 (BUD=0)"), "{one}");
    let rows = std::fs::read_to_string(&csv).unwrap();
    assert_eq!(rows.lines().count(), 1 + 3 + 1, "{rows}");
    // In full, every seed runs to its end.
    let (ok, out, err) = wmc(&["lab", goal, "--set", "BUD=0,50", "--full"]);
    assert!(
        ok && out
            .contains("variant 2   BUD=50  FAIL  0 of 3 seeds held every expectation, 3 needed\n"),
        "{out}{err}"
    );
    // One value: any command takes it.
    let (ok, out, err) = wmc(&["scenario", goal, "--set", "BUD=0"]);
    assert!(ok && out.contains("3 of 3 seeds held"), "{out}{err}");
    let (ok, _, err) = wmc(&["scenario", goal, "--set", "BUD=0,50"]);
    assert!(!ok && err.contains("a list of values is for lab"), "{err}");
    let (ok, _, err) = wmc(&["scenario", goal, "--set", "BUDS=1"]);
    assert!(
        !ok && err.contains("`BUDS`: the rules declare no such const (they declare: BUD)"),
        "{err}"
    );
    let (ok, _, err) = wmc(&["scenario", goal, "--jobs", "2"]);
    assert!(!ok && err.contains("only for lab"), "{err}");
    let (ok, _, err) = wmc(&["lab", goal, "--set", "BUD=x"]);
    assert!(
        !ok && err.contains("`x` is not a number or a time"),
        "{err}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// `wmc rates`: the rules in day units, every kind or those `--kind`
/// names; a pack's const set with `--set` shows in the numbers.
#[test]
fn rates_turn_rules_into_day_units() {
    let (ok, out, err) = wmc(&["rates", "--kind", "chicken"]);
    assert!(ok, "{err}");
    assert!(out.starts_with("chicken (extends "), "{out}");
    assert!(out.contains(": 5400 thinks a day (every 4 ticks)"), "{out}");
    let egg = out.lines().find(|l| l.contains("spawn egg at c")).unwrap();
    assert!(
        egg.contains("spawn egg") && egg.contains("0.2%") && egg.contains("10.8"),
        "{egg}"
    );
    let (ok, _, err) = wmc(&["rates", "--kind", "wolf"]);
    assert!(
        !ok && err.contains("--kind wolf: the rules have no such kind"),
        "{err}"
    );
    let dir = scratch("rates");
    let pack = dir.join("blob.rules");
    std::fs::write(
        &pack,
        "const BUD = 50\nkind blob { glyph \"b\"  cadence 16\n  when chance(BUD) and nearest free within 1 as c => spawn blob at c }\n",
    )
    .unwrap();
    let pack = pack.to_str().unwrap();
    let (ok, out, _) = wmc(&["rates", pack]);
    assert!(ok && out.contains("50%") && out.contains("675"), "{out}");
    let (ok, out, _) = wmc(&["rates", pack, "--set", "BUD=10"]);
    assert!(ok && out.contains("10%") && out.contains("135"), "{out}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// A start that does not fit the rules is refused at its line, by `lint
/// --scenario`, `scenario` and a new world alike: the `start` line, or the
/// map row that draws it (not quoted as a `start` line the file lacks),
/// with a need in its units.
#[test]
fn a_start_that_does_not_fit_names_its_line() {
    let dir = scratch("start-line");
    let f = dir.join("hungry.scenario");
    std::fs::write(
        &f,
        "seed 3\noutside soil\nmap {\n  ...\n  .F.\n}\nlegend {\n  . soil\n  F fox with (food = 30h)\n}\n",
    )
    .unwrap();
    let f = f.to_str().unwrap();
    let want =
        format!("{f}:5: `fox` at (1, 1) with `food = 27000 (30h)`: `food` holds 0 to 21600 (1d)");
    let rules = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../rules");
    let save = dir.join("save");
    for args in [
        vec!["lint", rules.to_str().unwrap(), "--scenario", f],
        vec!["scenario", f],
        vec!["run", save.to_str().unwrap(), "1", "--scenario", f],
    ] {
        let (ok, out, err) = wmc(&args);
        assert!(!ok && err.contains(&want), "{args:?}: {out}{err}");
    }
    std::fs::write(
        dir.join("two.scenario"),
        "seed 3\nstart hive 1 / 2\nstart wolf 1 / 4\n",
    )
    .unwrap();
    let two = dir.join("two.scenario");
    let (ok, _, err) = wmc(&["scenario", two.to_str().unwrap()]);
    assert!(
        !ok && err.contains(&format!(
            "{}:3: the scenario starts kinds the rules do not define: wolf",
            two.display()
        )),
        "{err}"
    );
    std::fs::remove_dir_all(&dir).unwrap();
}

// ---- odd command lines -------------------------------------------------------------------

/// `wmc args` run in `dir` with `env` set (and no other `WMC_RULES` or
/// `WMC_THREADS`): exit code (`None`: killed by a signal), stdout, stderr.
fn wmc_in(dir: &Path, env: &[(&str, &str)], args: &[&str]) -> (Option<i32>, String, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_wmc"))
        .env_remove("WMC_RULES")
        .env_remove("WMC_THREADS")
        .envs(env.iter().copied())
        .current_dir(dir)
        .args(args)
        .output()
        .expect("wmc runs");
    let text = |b: &[u8]| String::from_utf8_lossy(b).into_owned();
    (out.status.code(), text(&out.stdout), text(&out.stderr))
}

/// `wmc args` exits 0, or 1 with an `Error:`; never a panic or a signal.
fn no_panic(dir: &Path, env: &[(&str, &str)], args: &[&str]) {
    let (code, out, err) = wmc_in(dir, env, args);
    let said = format!("{env:?} {args:?}: exit {code:?}\n{out}{err}");
    assert!(!err.contains("panicked at"), "{said}");
    match code {
        Some(0) => {}
        Some(1) => assert!(err.contains("Error: "), "{said}"),
        _ => panic!("{said}"),
    }
}

/// A scratch directory holding what odd command lines name: a tiny
/// scenario and a broken one, a rules file that does not compile, a file
/// where a save should be, a save with a world file of garbage, a real save
/// whose camera is past the end of the world, and one whose chunk file is
/// cut short.
fn odd_fixtures(name: &str) -> PathBuf {
    use sim_core::{Scenario, Store, sim};
    let dir = scratch(name);
    let tiny = "seed 1\nsize 1 1\nstart fox at (0, 0)\nrun 3\nexpect count fox == 1\n";
    std::fs::write(dir.join("tiny.scenario"), tiny).unwrap();
    std::fs::write(dir.join("bad.scenario"), "seed x\n").unwrap();
    std::fs::write(dir.join("bad.rules"), "kind {\n").unwrap();
    std::fs::write(dir.join("afile"), "not a save").unwrap();
    std::fs::write(
        dir.join("crab.sprite"),
        "palette { s \"#d9623b\" }\nsprite crab { frames 2 every 1s \"O\" at 0.5 0.5 ~x 0.1 color s pose 1 { } }\nsprite fox { \"f\" at 1.5 0.5 }\n",
    )
    .unwrap();
    std::fs::write(dir.join("bad.sprite"), "sprite crab {\n").unwrap();
    std::fs::create_dir_all(dir.join("garbage/chunks")).unwrap();
    std::fs::write(dir.join("garbage/world.wmc"), "WMCW and then nothing").unwrap();
    for save in ["far", "broken"] {
        let store = Store::open(dir.join(save)).unwrap();
        let mut w = sim::new_world(&Scenario::parse("t", tiny).unwrap());
        sim::save(&mut w, &store).unwrap();
    }
    std::fs::write(dir.join("far/camera.txt"), "1e300 -1e300\n").unwrap();
    let chunk = dir.join("broken/chunks/0_0.wmcc");
    let bytes = std::fs::read(&chunk).unwrap();
    std::fs::write(&chunk, &bytes[..bytes.len() / 2]).unwrap();
    dir
}

/// Missing and extra arguments, unknown flags, numbers out of every range,
/// empty strings, paths that are not there or not what they should be: an
/// error that says so, or a run, never a panic. (`play` only where it
/// fails before it opens a window.)
#[test]
fn odd_command_lines_never_panic() {
    let dir = odd_fixtures("odd");
    let rules = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../rules");
    let rules = rules.to_str().unwrap();
    // No arguments at all is the menu, a window: not here.
    let cases: &[&[&str]] = &[
        &[""],
        &["nope"],
        &["SHOW"],
        &["-v"],
        &["--", "show"],
        &["--strict"],
        &["show", ""],
        &["show", "-1", "5"],
        &["show", "5", "5", "-1"],
        &["show", "0", "5"],
        &["show", "5", "0"],
        &["show", "4097", "4097"],
        &["show", "65", "1", "18446744073709551615"],
        &["show", "5", "5", "18446744073709551616"],
        &["show", "99999999999", "1"],
        &["show", "1", "2", "3", "4"],
        &["show", " 1"],
        &["show", "1e3"],
        &["show", "--threads"],
        &["show", "--threads", "0"],
        &["show", "--threads", "-1"],
        &["show", "--threads", "x"],
        &["show", "--threads", ""],
        &["show", "--threads", "99999999999999999999"],
        &["show", "3", "3", "--threads", "1", "--threads", "2"],
        &["show", "--rules"],
        &["show", "--rules", ""],
        &["show", "--rules", "/nonexistent"],
        &["show", "--rules", "bad.rules"],
        &["show", "--rules", "afile"],
        &["show", "--rules", "."],
        &["show", "--scenario"],
        &["show", "--scenario", ""],
        &["show", "--scenario", "."],
        &["show", "--scenario", "/nonexistent"],
        &["show", "--scenario", "bad.scenario"],
        &["show", "--scenario", "tiny.scenario"],
        &["show", "2", "2", "--scenario", "tiny.scenario"],
        &[
            "show",
            "--scenario",
            "tiny.scenario",
            "--scenario",
            "bad.scenario",
        ],
        &["show", "--strict"],
        &["show", "--seed", "1"],
        &["show", "-v"],
        &["run"],
        &["run", "s1"],
        &["run", "s1", "-1"],
        &["run", "s1", "x"],
        &["run", "s1", "0"],
        &["run", "s1", "18446744073709551616"],
        &["run", "s1", "3", "0", "0"],
        &["run", "s1", "1", "1", "1", "1", "extra"],
        &["run", "", "1"],
        &["run", "afile", "1"],
        &["run", "garbage", "1"],
        &["run", "far", "3"],
        &["run", "broken", "1"],
        &["run", "far", "1", "--rules", "bad.rules"],
        &["why"],
        &["why", "-v"],
        &["why", "-v", "s2"],
        &["why", "-v", "-v", "s2", "0", "0"],
        &["why", "s2", "1"],
        &["why", "s2", "1", "x"],
        &["why", "s2", "99999999999", "1"],
        &["why", "s2", "-2147483648", "2147483647"],
        &["why", "s2", "1", "1", "-5"],
        &["why", "far", "0", "0"],
        &["why", "garbage", "0", "0"],
        &["why", "broken", "0", "0"],
        &["why", "s3", "0", "0", "0", "--scenario", "tiny.scenario"],
        &[
            "why",
            "-v",
            "s4",
            "0",
            "0",
            "2",
            "1",
            "1",
            "--scenario",
            "tiny.scenario",
        ],
        &["why", "s5", "0", "0", "0", "1", "1", "1", "1"],
        &["play"],
        &["play", "s6", "x"],
        &["play", "s6", "0", "0"],
        &["play", "s6", "1", "1", "1", "1"],
        &["lint"],
        &["lint", ""],
        &["lint", "/nonexistent"],
        &["lint", "bad.rules"],
        &["lint", "afile"],
        &["lint", rules],
        &["lint", rules, rules],
        &["lint", "--strict", rules, "--scenario", "tiny.scenario"],
        &["lint", "--scenario", "tiny.scenario"],
        &["lint", rules, "--scenario", "bad.scenario"],
        &["lint", rules, "--scenario", "/nonexistent"],
        &["scenario"],
        &["scenario", ""],
        &["scenario", "."],
        &["scenario", "/nonexistent"],
        &["scenario", "bad.scenario"],
        &["scenario", "afile"],
        &["scenario", "tiny.scenario"],
        &["scenario", "tiny.scenario", "--threads", "1"],
        &["scenario", "tiny.scenario", "--rules", "bad.rules"],
        &["scenario", "tiny.scenario", "tiny.scenario"],
        &["observe"],
        &["observe", ""],
        &["observe", "0"],
        &["observe", "-1"],
        &["observe", "1x"],
        &["observe", "99999999999d"],
        &["observe", "3", "--scenario", "tiny.scenario"],
        &["observe", "3", "2", "2", "--scenario", "tiny.scenario"],
        &["observe", "3", "1", "1", "1", "extra"],
        &["observe", "3", "--every"],
        &["observe", "3", "--every", "0"],
        &[
            "observe",
            "3",
            "--every",
            "2h",
            "--scenario",
            "tiny.scenario",
        ],
        &["observe", "3", "--sample", "-1"],
        &["observe", "3", "--sample", "100001"],
        &[
            "observe",
            "3",
            "--sample",
            "0",
            "--scenario",
            "tiny.scenario",
        ],
        &["observe", "3", "--kind", "nobody"],
        &["observe", "3", "--csv", ""],
        &["observe", "3", "--csv", "."],
        &["observe", "3", "--scenario", "bad.scenario"],
        &["observe", "3", "--rules", "bad.rules"],
        &["show", "--every", "1h"],
        &["run", "s1", "1", "--csv", "x.csv"],
        &["scenario", "tiny.scenario", "--seeds", "0"],
        &["scenario", "tiny.scenario", "--seeds", "1001"],
        &["scenario", "tiny.scenario", "--seeds", "2"],
        &["scenario", "tiny.scenario", "--seeds", "2", "--pass", "3"],
        &["scenario", "tiny.scenario", "--pass", "1"],
        &["scenario", "tiny.scenario", "--pass", "x"],
        &["run", "s1", "1", "--seeds", "2"],
        &["lab"],
        &["lab", ""],
        &["lab", "/nonexistent"],
        &["lab", "bad.scenario"],
        &["lab", "tiny.scenario"],
        &["lab", "tiny.scenario", "tiny.scenario"],
        &["lab", "tiny.scenario", "--set", "X=1,2"],
        &["lab", "tiny.scenario", "--jobs", "0"],
        &["lab", "tiny.scenario", "--seeds", "2", "--pass", "3"],
        &["show", "--set", "=1"],
        &["show", "--set", "1X=1"],
        &["show", "--set", "X"],
        &["show", "--set", "X=1", "--set", "X=2"],
        &["show", "--set", "X=99999999999"],
        &["run", "s1", "1", "--full"],
        &["rates"],
        &["rates", ""],
        &["rates", "/nonexistent"],
        &["rates", "bad.rules"],
        &["rates", "afile"],
        &["rates", rules],
        &["rates", "--kind"],
        &["rates", "--kind", "nobody"],
        &["rates", "--scenario", "tiny.scenario"],
        &["run", "s1", "1", "--kind", "fox"],
        &["sprite"],
        &["sprite", "draw"],
        &["sprite", "render"],
        &["sprite", "render", "crab.sprite"],
        &["sprite", "render", "crab.sprite", "--png", "o.png"],
        &[
            "sprite",
            "render",
            "crab.sprite",
            "--png",
            "o.png",
            "--rules",
            rules,
        ],
        &[
            "sprite",
            "render",
            "crab.sprite",
            "--png",
            "o.png",
            "--sprite",
            "nope",
        ],
        &[
            "sprite",
            "render",
            "crab.sprite",
            "--png",
            "o.png",
            "--pose",
            "1",
        ],
        &[
            "sprite",
            "render",
            "crab.sprite",
            "--png",
            "o.png",
            "--pose",
            "256",
        ],
        &[
            "sprite",
            "render",
            "crab.sprite",
            "--png",
            "o.png",
            "--cell",
            "3",
        ],
        &[
            "sprite",
            "render",
            "crab.sprite",
            "--png",
            "o.png",
            "--cell",
            "257",
        ],
        &[
            "sprite",
            "render",
            "crab.sprite",
            "--png",
            "o.png",
            "--cell",
            "256",
        ],
        &[
            "sprite",
            "render",
            "crab.sprite",
            "--png",
            "o.png",
            "--bg",
            "red",
        ],
        &[
            "sprite",
            "render",
            "crab.sprite",
            "--png",
            "",
            "--bg",
            "#000000",
        ],
        &["sprite", "render", "crab.sprite", "--png", "."],
        &[
            "sprite",
            "render",
            "crab.sprite",
            "--png",
            "/nonexistent/o.png",
        ],
        &["sprite", "render", "bad.sprite", "--png", "o.png"],
        &["sprite", "render", "afile", "--png", "o.png"],
        &["sprite", "render", "/nonexistent", "--png", "o.png"],
        &[
            "sprite",
            "render",
            "crab.sprite",
            "crab.sprite",
            "--png",
            "o.png",
        ],
        &["show", "--png", "o.png"],
        &["lint", rules, "--cell", "24"],
        &["sprite", "score"],
        &["sprite", "score", "crab.sprite"],
        &["sprite", "score", "crab.sprite", "--target", "o.png"],
        &[
            "sprite",
            "score",
            "crab.sprite",
            "--target",
            "o.png",
            "--sprite",
            "crab",
        ],
        &[
            "sprite",
            "score",
            "crab.sprite",
            "--target",
            "afile",
            "--sprite",
            "crab",
        ],
        &[
            "sprite",
            "score",
            "crab.sprite",
            "--target",
            "/nonexistent",
            "--sprite",
            "crab",
        ],
        &[
            "sprite",
            "score",
            "crab.sprite",
            "--target",
            "crab.sprite",
            "--sprite",
            "crab",
        ],
        &[
            "sprite",
            "score",
            "crab.sprite",
            "--target",
            "o.png",
            "--sprite",
            "crab",
            "--key",
            "x",
        ],
        &[
            "sprite",
            "score",
            "crab.sprite",
            "--target",
            "o.png",
            "--sprite",
            "crab",
            "--diff",
            "d.png",
        ],
        &[
            "sprite",
            "score",
            "crab.sprite",
            "--target",
            "o.png",
            "--sprite",
            "crab",
            "--diff",
            "/nonexistent/d.png",
        ],
        &["sprite", "score", "crab.sprite", "--png", "o.png"],
        &[
            "sprite",
            "render",
            "crab.sprite",
            "--png",
            "o.png",
            "--target",
            "o.png",
        ],
        &["sprite", "score", "bad.sprite", "--target", "o.png"],
        &["sprite", "fit"],
        &["sprite", "fit", "o.png"],
        &["sprite", "fit", "o.png", "--kind", "crab"],
        &[
            "sprite", "fit", "o.png", "--kind", "crab", "-o", "f.sprite", "--stamps", "2",
        ],
        &["sprite", "fit", "afile", "--kind", "crab", "-o", "f.sprite"],
        &[
            "sprite",
            "fit",
            "/nonexistent",
            "--kind",
            "crab",
            "-o",
            "f.sprite",
        ],
        &[
            "sprite", "fit", "o.png", "--kind", "crab", "-o", "f.sprite", "--stamps", "0",
        ],
        &[
            "sprite", "fit", "o.png", "--kind", "crab", "-o", "f.sprite", "--stamps", "33",
        ],
        &[
            "sprite", "fit", "o.png", "--kind", "crab", "-o", "f.sprite", "--colors", "9",
        ],
        &[
            "sprite", "fit", "o.png", "--kind", "crab", "-o", "f.sprite", "--chars", "",
        ],
        &[
            "sprite", "fit", "o.png", "--kind", "crab", "-o", "f.sprite", "--chars", "a b",
        ],
        &[
            "sprite", "fit", "o.png", "--kind", "crab", "-o", "f.sprite", "--size", "2",
        ],
        &[
            "sprite", "fit", "o.png", "--kind", "crab", "-o", "f.sprite", "--size", "0", "1",
        ],
        &[
            "sprite", "fit", "o.png", "--kind", "crab", "-o", "f.sprite", "--seed", "-1",
        ],
        &[
            "sprite", "fit", "o.png", "--kind", "fox", "-o", "f.sprite", "--stamps", "1",
            "--rules", rules,
        ],
        &[
            "sprite", "fit", "o.png", "--kind", "eel", "-o", "f.sprite", "--rules", rules,
        ],
        &[
            "sprite",
            "fit",
            "o.png",
            "--kind",
            "crab",
            "-o",
            "/nonexistent/f.sprite",
            "--stamps",
            "1",
        ],
        &[
            "sprite",
            "render",
            "crab.sprite",
            "--png",
            "o.png",
            "--stamps",
            "2",
        ],
        &["show", "-o", "x"],
        &["show", "--size", "1", "1"],
    ];
    for args in cases {
        no_panic(&dir, &[], args);
    }
    for rules in ["", ":", "/nonexistent", "bad.rules", "afile"] {
        no_panic(&dir, &[("WMC_RULES", rules)], &["show", "3", "3"]);
        no_panic(&dir, &[("WMC_RULES", rules)], &["run", "far", "1"]);
    }
    std::fs::remove_dir_all(&dir).unwrap();
}

/// Cases for `generated_command_lines_never_panic`: a few by default,
/// `WMC_FUZZ_CASES` for `make fuzz`, `WMC_FUZZ_SEED` for others.
fn fuzz_config(cases: u32) -> proptest::test_runner::Config {
    let var = |name: &str| std::env::var(name).ok();
    let seed = var("WMC_FUZZ_SEED").and_then(|v| v.parse().ok());
    proptest::test_runner::Config {
        cases: var("WMC_FUZZ_CASES")
            .and_then(|v| v.parse().ok())
            .unwrap_or(cases),
        failure_persistence: None,
        rng_seed: proptest::test_runner::RngSeed::Fixed(seed.unwrap_or(0x5EED)),
        ..proptest::test_runner::Config::default()
    }
}

/// What a generated command line is made of: no `play` or `menu` (a
/// window), and no number a tick count reads as long.
const WORDS: &[&str] = &[
    "show",
    "run",
    "why",
    "lint",
    "scenario",
    "sprite",
    "render",
    "--png",
    "--sprite",
    "--pose",
    "--cell",
    "--bg",
    "crab.sprite",
    "bad.sprite",
    "o.png",
    "crab",
    "#00ff00",
    "score",
    "--target",
    "--diff",
    "--key",
    "fit",
    "-o",
    "f.sprite",
    "--stamps",
    "--colors",
    "--chars",
    "--size",
    "--kind",
    "--threads",
    "--rules",
    "--scenario",
    "--strict",
    "-v",
    "--seed",
    "--",
    "",
    ".",
    "s7",
    "garbage",
    "far",
    "broken",
    "afile",
    "tiny.scenario",
    "bad.scenario",
    "bad.rules",
    "/nonexistent",
    "RULES",
    "0",
    "1",
    "2",
    "3",
    "64",
    "65",
    "-1",
    "-0",
    "x",
    "1e3",
    " 1",
    "4097",
    "-2147483649",
    "18446744073709551616",
    "99999999999999999999",
    "é",
    "\u{feff}1",
];

/// Any list of these words is an error that says so, or a run.
#[test]
fn generated_command_lines_never_panic() {
    let dir = odd_fixtures("generated");
    let rules = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../rules");
    let rules = rules.to_str().unwrap();
    let words = proptest::collection::vec(proptest::sample::select(WORDS), 0..8);
    let mut runner = proptest::test_runner::TestRunner::new(fuzz_config(24));
    let result = runner.run(&words, |words| {
        let args: Vec<&str> = words
            .iter()
            .map(|w| if *w == "RULES" { rules } else { w })
            .collect();
        // No words at all is the menu: a window.
        if args.is_empty() {
            return Ok(());
        }
        no_panic(&dir, &[], &args);
        Ok(())
    });
    std::fs::remove_dir_all(&dir).unwrap();
    result.unwrap();
}

/// A `WMC_THREADS` that is no thread count is an error that names it,
/// before any world is made; `--threads` overrides it.
#[test]
fn a_bad_wmc_threads_is_an_error_and_threads_overrides_it() {
    let dir = odd_fixtures("threads-env");
    for v in ["0", "-1", "abc", "1.5"] {
        for args in [
            &["show", "3", "3"][..],
            &["run", "far", "1"],
            &["why", "far", "0", "0"],
            &["scenario", "tiny.scenario"],
        ] {
            let (code, out, err) = wmc_in(&dir, &[("WMC_THREADS", v)], args);
            let want = format!("Error: WMC_THREADS=`{v}` is not a thread count");
            assert!(
                code == Some(1) && err.contains(&want),
                "{v} {args:?}: {code:?}\n{out}{err}"
            );
        }
        let (code, out, err) = wmc_in(
            &dir,
            &[("WMC_THREADS", v)],
            &["scenario", "tiny.scenario", "--threads", "2"],
        );
        assert_eq!(code, Some(0), "{v}: {out}{err}");
        assert!(out.contains("(2 threads)"), "{out}");
    }
    std::fs::remove_dir_all(&dir).unwrap();
}

/// More threads than the system will start is an error, not a panic where
/// the pool fails to spawn them; 64 (the determinism gate's) run.
#[test]
fn a_thread_count_past_the_most_is_an_error() {
    let dir = odd_fixtures("threads-max");
    let (code, out, err) = wmc_in(
        &dir,
        &[],
        &["scenario", "tiny.scenario", "--threads", "100000"],
    );
    assert!(
        code == Some(1) && err.contains("--threads needs a count from 1 to 1024"),
        "{code:?}\n{out}{err}"
    );
    let (code, out, err) = wmc_in(
        &dir,
        &[("WMC_THREADS", "100000")],
        &["scenario", "tiny.scenario"],
    );
    assert!(
        code == Some(1) && err.contains("WMC_THREADS=`100000` is not a thread count (1 to 1024)"),
        "{code:?}\n{out}{err}"
    );
    for env in [&[][..], &[("WMC_THREADS", "64")]] {
        let args: &[&str] = if env.is_empty() {
            &["scenario", "tiny.scenario", "--threads", "64"]
        } else {
            &["scenario", "tiny.scenario"]
        };
        let (code, out, err) = wmc_in(&dir, env, args);
        assert!(
            code == Some(0) && out.contains("(64 threads)"),
            "{code:?}\n{out}{err}"
        );
    }
    std::fs::remove_dir_all(&dir).unwrap();
}

/// An argument that is not UTF-8 is an error that says which, not a panic
/// where the arguments are read; a `WMC_THREADS` that is not is no count,
/// not unset.
#[test]
fn arguments_that_are_not_utf8_are_errors() {
    use std::ffi::OsStr;
    use std::os::unix::ffi::OsStrExt;
    let dir = odd_fixtures("utf8");
    let bad = OsStr::from_bytes(b"x\xff.scenario");
    for args in [
        vec![OsStr::new("show"), bad],
        vec![OsStr::new("scenario"), bad],
        vec![bad],
        vec![OsStr::new("show"), OsStr::new("--rules"), bad],
    ] {
        let out = Command::new(env!("CARGO_BIN_EXE_wmc"))
            .env_remove("WMC_RULES")
            .env_remove("WMC_THREADS")
            .current_dir(&dir)
            .args(&args)
            .output()
            .unwrap();
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(
            out.status.code() == Some(1)
                && err.contains("Error: argument \"x\\xFF.scenario\" is not UTF-8"),
            "{args:?}: {:?}\n{err}",
            out.status
        );
    }
    let out = Command::new(env!("CARGO_BIN_EXE_wmc"))
        .env_remove("WMC_RULES")
        .env("WMC_THREADS", OsStr::from_bytes(b"\xff"))
        .current_dir(&dir)
        .args(["scenario", "tiny.scenario"])
        .output()
        .unwrap();
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.code() == Some(1)
            && err.contains("Error: WMC_THREADS=`\u{FFFD}` is not a thread count"),
        "{:?}\n{err}",
        out.status
    );
    std::fs::remove_dir_all(&dir).unwrap();
}
// ---- what the tools report -------------------------------------------------------------

/// A pack of a group (`herd`, 5 heads of 12) and a big kind (`hill`, 2x2),
/// and a scenario starting one of each: what `run` and `why` say of them.
fn herd_fixtures(name: &str) -> PathBuf {
    let dir = scratch(name);
    std::fs::create_dir_all(dir.join("herd")).unwrap();
    std::fs::write(
        dir.join("herd/herd.rules"),
        "kind herd { glyph \"h\"  group max 12 init 5  when true => idle }\n\
         kind hill { glyph \"H\"  size 2 2  when true => idle }\n\
         kind sloth { glyph \"s\"  cadence 1073741824  when true => idle }\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("herd.scenario"),
        "rules herd\nseed 1\nsize 16 16\nterrain water_level 0 rock_on_soil 0 rock_on_water 0\n\
         start herd at (3, 3)\nstart hill at (8, 8)\nstart sloth at (12, 12)\n",
    )
    .unwrap();
    dir
}

/// `run` counts heads alive, as born and died do (a group of 5 is 5), and
/// says how many rows hold them; `why` shows a group's heads and a big
/// actor's size.
#[test]
fn run_counts_heads_and_why_shows_what_a_row_stands_for() {
    let dir = herd_fixtures("heads");
    let (code, out, err) = wmc_in(
        &dir,
        &[],
        &["run", "save", "0", "--scenario", "herd.scenario"],
    );
    assert_eq!(code, Some(0), "{out}{err}");
    assert!(
        out.contains("(7 actors in 3 rows: herd 5, hill 1, sloth 1)"),
        "{out}"
    );
    let herd = out
        .lines()
        .find(|l| l.trim_start().starts_with("herd "))
        .unwrap();
    let alive: Vec<&str> = herd.split_whitespace().collect();
    assert_eq!(alive[1], "5", "{herd}");
    let (code, out, err) = wmc_in(
        &dir,
        &[],
        &["why", "save", "3", "3", "--scenario", "herd.scenario"],
    );
    assert_eq!(code, Some(0), "{out}{err}");
    assert!(out.contains(", heads 5 (group max 12)\n"), "{out}");
    let (code, out, err) = wmc_in(
        &dir,
        &[],
        &["why", "save", "8", "8", "--scenario", "herd.scenario"],
    );
    assert_eq!(code, Some(0), "{out}{err}");
    assert!(out.contains(", size 2x2\n"), "{out}");
    std::fs::remove_dir_all(&dir).unwrap();
}

/// `why` on a kind that thinks once in 2^30 ticks says when its think comes
/// instead of stepping the world for hours to reach it.
#[test]
fn why_does_not_wait_out_a_huge_cadence() {
    let dir = herd_fixtures("sloth");
    let started = std::time::Instant::now();
    let (code, out, err) = wmc_in(
        &dir,
        &[],
        &["why", "save", "12", "12", "--scenario", "herd.scenario"],
    );
    assert_eq!(code, Some(1), "{out}{err}");
    assert!(
        err.contains("it thinks every 1073741824 ticks, its next think ")
            && err.contains("`why` waits at most 65536 for one: give `why` "),
        "{err}"
    );
    assert!(started.elapsed().as_secs() < 60, "it gave up at once");
    std::fs::remove_dir_all(&dir).unwrap();
}

/// `run`, `why` and `observe` take a time as a scenario's `run` line
/// writes one, and `observe --every` the unit its table prints.
#[test]
fn time_arguments_take_units() {
    let dir = odd_fixtures("times");
    let (code, out, err) = wmc_in(
        &dir,
        &[],
        &["run", "fresh", "2h", "--scenario", "tiny.scenario"],
    );
    assert_eq!(code, Some(0), "{out}{err}");
    assert!(out.starts_with("ticks:     1800 "), "{out}");
    let (code, out, err) = wmc_in(
        &dir,
        &[],
        &[
            "why",
            "fresh",
            "0",
            "0",
            "30min",
            "--scenario",
            "tiny.scenario",
        ],
    );
    // It stepped half an hour from dawn (tick 5400), where the lone fox
    // has died.
    assert!(
        code == Some(1) && err.contains("nobody at (0, 0) at tick 5850"),
        "{out}{err}"
    );
    let (code, out, err) = wmc_in(
        &dir,
        &[],
        &[
            "observe",
            "1h",
            "--every",
            "30min",
            "--scenario",
            "tiny.scenario",
        ],
    );
    assert_eq!(code, Some(0), "{out}{err}");
    for args in [
        &["run", "fresh", "2x"][..],
        &["why", "fresh", "0", "0", "2m"],
        &["observe", "0"],
    ] {
        let (code, out, err) = wmc_in(&dir, &[], args);
        assert!(
            code == Some(1) && err.contains("bad time"),
            "{args:?}: {out}{err}"
        );
    }
    std::fs::remove_dir_all(&dir).unwrap();
}

/// A flag missing its value does not take the next flag as it: `--csv
/// --gates` writes no file named `--gates`.
#[test]
fn a_flag_never_takes_another_flag_as_its_value() {
    let dir = odd_fixtures("flag-value");
    let (code, out, err) = wmc_in(
        &dir,
        &[],
        &[
            "observe",
            "1h",
            "--scenario",
            "tiny.scenario",
            "--csv",
            "--gates",
        ],
    );
    assert!(
        code == Some(1) && err.contains("--csv needs a value"),
        "{out}{err}"
    );
    assert!(!dir.join("--gates").exists());
    std::fs::remove_dir_all(&dir).unwrap();
}

/// `observe --csv` and `lab --csv` open their file before the run: a path
/// they cannot write fails at once, before any world steps.
#[test]
fn a_csv_that_cannot_be_written_fails_before_the_run() {
    let dir = odd_fixtures("csv-first");
    let csv = "no/such/dir/out.csv";
    for args in [
        &["observe", "4d", "--scenario", "tiny.scenario", "--csv", csv][..],
        &["lab", "tiny.scenario", "--csv", csv],
    ] {
        let (code, out, err) = wmc_in(&dir, &[], args);
        assert!(
            code == Some(1) && err.contains(&format!("writing {csv}")),
            "{args:?}: {out}{err}"
        );
        assert!(
            !out.contains("observe ") && !out.contains("variant "),
            "nothing ran: {out}"
        );
    }
    std::fs::remove_dir_all(&dir).unwrap();
}

/// `lab` and a scenario on several seeds run a world a thread unless
/// `--threads` or `WMC_THREADS` says otherwise; `lint` and `rates` run no
/// world, and a bad `WMC_THREADS` does not stop them.
#[test]
fn many_worlds_at_once_honour_wmc_threads() {
    let dir = odd_fixtures("threads-many");
    std::fs::write(
        dir.join("three.scenario"),
        "seed 1\nseeds 3\nsize 1 1\nstart fox at (0, 0)\nrun 3\nexpect count fox == 1\n",
    )
    .unwrap();
    let (code, out, err) = wmc_in(
        &dir,
        &[("WMC_THREADS", "2")],
        &["scenario", "three.scenario"],
    );
    assert!(code == Some(0) && out.contains("(2 threads)"), "{out}{err}");
    let (code, out, err) = wmc_in(&dir, &[], &["scenario", "three.scenario"]);
    assert!(code == Some(0) && out.contains("(1 threads)"), "{out}{err}");
    let (code, out, err) = wmc_in(&dir, &[("WMC_THREADS", "2")], &["lab", "three.scenario"]);
    assert!(
        code == Some(0) && out.contains(", 2 thread a world;"),
        "{out}{err}"
    );
    let rules = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../rules");
    for args in [
        &["lint", rules.to_str().unwrap()][..],
        &["rates", "--kind", "fox"],
    ] {
        let (code, out, err) = wmc_in(&dir, &[("WMC_THREADS", "abc")], args);
        assert_eq!(code, Some(0), "{args:?}: {out}{err}");
    }
    std::fs::remove_dir_all(&dir).unwrap();
}

/// On several seeds, a world that cannot be made says which seed; a file's
/// `pass M` without `seeds` is checked against the seeds it runs on, so
/// `--seeds N` can cover it.
#[test]
fn several_seeds_name_the_seed_and_take_pass_from_the_file() {
    let dir = odd_fixtures("seeds");
    std::fs::write(
        dir.join("wolf.scenario"),
        "seed 5\nseeds 2\nsize 4 4\nstart wolf at (0, 0)\nrun 3\nexpect count fox == 0\n",
    )
    .unwrap();
    let (code, out, err) = wmc_in(&dir, &[], &["scenario", "wolf.scenario"]);
    assert!(
        code == Some(1)
            && err.contains(
                "wolf.scenario:4: seed 5: the scenario starts kinds the rules do not define: wolf"
            ),
        "{out}{err}"
    );
    std::fs::write(
        dir.join("pass.scenario"),
        "seed 1\npass 2\nsize 1 1\nstart fox at (0, 0)\nrun 3\nexpect count fox == 1\n",
    )
    .unwrap();
    let (code, out, err) = wmc_in(&dir, &[], &["scenario", "pass.scenario", "--seeds", "3"]);
    assert!(
        code == Some(0) && out.contains("3 of 3 seeds held every expectation, 2 needed"),
        "{out}{err}"
    );
    let (code, out, err) = wmc_in(&dir, &[], &["scenario", "pass.scenario"]);
    assert!(
        code == Some(1) && err.contains("it needs 2 seeds to pass, but runs on 1"),
        "{out}{err}"
    );
    std::fs::remove_dir_all(&dir).unwrap();
}

/// `sprite render` writes a sheet of every pose's frames at the cell size
/// and 8x, says what each row is, and takes the kinds' sizes from the rules.
#[test]
fn sprite_render_writes_a_sheet() {
    let dir = odd_fixtures("sprite-render");
    let (code, out, err) = wmc_in(
        &dir,
        &[],
        &[
            "sprite",
            "render",
            "crab.sprite",
            "--png",
            "o.png",
            "--cell",
            "10",
        ],
    );
    assert_eq!(code, Some(0), "{out}{err}");
    assert!(
        out.contains("row 1: crab (1 x 1): own stamps, 2 frames every 1000ms, 1 stamps"),
        "{out}"
    );
    assert!(
        out.contains("row 2: crab (1 x 1): pose 1, 2 frames every 1000ms, 0 stamps"),
        "{out}"
    );
    // The fox's stamp reaches into a second cell.
    assert!(
        out.contains("row 3: fox (2 x 1): own stamps, a still, 1 stamps"),
        "{out}"
    );
    let png = std::fs::read(dir.join("o.png")).unwrap();
    assert_eq!(&png[1..4], b"PNG");
    // Widest row: the fox's 20 px enlarged 8x, or the crab's two frames of 80
    // and a gap; 8 px margins; three rows of h * 9 + 3 gaps of 6.
    let w = u32::from_be_bytes(png[16..20].try_into().unwrap());
    let h = u32::from_be_bytes(png[20..24].try_into().unwrap());
    assert_eq!((w, h), (8 * 2 + 2 * 80 + 6, 8 * 2 + 3 * (10 * 9 + 18)));
    assert!(
        out.contains(&format!("wrote o.png: {w} x {h} pixels")),
        "{out}"
    );
    // With the rules the fox is one cell; one sprite, one pose.
    let rules = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../rules");
    let (code, out, _) = wmc_in(
        &dir,
        &[],
        &[
            "sprite",
            "render",
            "crab.sprite",
            "--png",
            "o.png",
            "--sprite",
            "fox",
            "--rules",
            rules.to_str().unwrap(),
        ],
    );
    assert_eq!(code, Some(0));
    assert_eq!(
        out.lines().next(),
        Some("row 1: fox (1 x 1): own stamps, a still, 1 stamps")
    );
    // A pose the sprite lacks shows its own stamps.
    let (_, out, _) = wmc_in(
        &dir,
        &[],
        &[
            "sprite",
            "render",
            "crab.sprite",
            "--png",
            "o.png",
            "--sprite",
            "crab",
            "--pose",
            "7",
        ],
    );
    assert_eq!(
        out.lines().next(),
        Some("row 1: crab (1 x 1): own stamps, 2 frames every 1000ms, 1 stamps")
    );
    for (args, want) in [
        (
            &["sprite", "render", "crab.sprite"][..],
            "sprite render needs --png <out.png>",
        ),
        (
            &["sprite", "render", "bad.sprite", "--png", "o.png"],
            "bad.sprite:1:13: this sprite's `{` is never closed",
        ),
        (
            &[
                "sprite",
                "render",
                "crab.sprite",
                "--png",
                "o.png",
                "--sprite",
                "eel",
            ],
            "crab.sprite: no sprite `eel` (it has: crab, fox)",
        ),
        (
            &[
                "sprite",
                "render",
                "crab.sprite",
                "--png",
                "o.png",
                "--cell",
                "3",
            ],
            "--cell needs a size in pixels from 4 to 256",
        ),
        (
            &["show", "--png", "o.png"],
            "--png, --sprite, --pose, --cell, --bg, --target, --diff, --key, -o, --stamps, --colors, --chars, --size and --seed are only for sprite",
        ),
    ] {
        let (code, out, err) = wmc_in(&dir, &[], args);
        assert!(
            code == Some(1) && err.contains(want),
            "{args:?}: {out}{err}"
        );
    }
    std::fs::remove_dir_all(&dir).unwrap();
}

/// `sprite score` gives the same numbers twice, 1 for the shape of a
/// sprite against its own render, and writes the side-by-side diff.
#[test]
fn sprite_score_measures_a_sprite_against_an_image() {
    use app::sprite::{parse, raster, sheet};
    let dir = odd_fixtures("sprite-score");
    let text = std::fs::read_to_string(dir.join("crab.sprite")).unwrap();
    let f = parse("crab.sprite", &text).unwrap();
    let own = raster::draw(
        &f.sprites[0].poses[0],
        0,
        &[0xd9623b],
        sim_core::rules::DEFAULT_COLOR,
        (1, 1),
        24,
    );
    sheet::write_png(&own, &dir.join("own.png")).unwrap();
    let args = [
        "sprite",
        "score",
        "crab.sprite",
        "--sprite",
        "crab",
        "--target",
        "own.png",
        "--diff",
        "d.png",
    ];
    let (code, out, err) = wmc_in(&dir, &[], &args);
    assert_eq!(code, Some(0), "{out}{err}");
    assert!(out.contains("shape   1.000"), "{out}");
    assert!(out.contains("stamps  1"), "{out}");
    assert!(out.contains("the subject: its alpha"), "{out}");
    assert!(out.contains("wrote d.png"), "{out}");
    let (_, again, _) = wmc_in(&dir, &[], &args);
    assert_eq!(again, out, "the same numbers twice");
    let png = std::fs::read(dir.join("d.png")).unwrap();
    assert_eq!(&png[1..4], b"PNG");
    for (args, want) in [
        (
            &["sprite", "score", "crab.sprite", "--target", "own.png"][..],
            "crab.sprite holds 2 sprites: name one with --sprite (crab, fox)",
        ),
        (
            &["sprite", "score", "crab.sprite", "--sprite", "crab"],
            "sprite score needs --target <image.png>",
        ),
        (
            &[
                "sprite",
                "score",
                "crab.sprite",
                "--sprite",
                "crab",
                "--target",
                "afile",
            ],
            "afile:",
        ),
        (
            &["sprite", "score", "crab.sprite", "--png", "o.png"],
            "--png is only for sprite render",
        ),
        (
            &[
                "sprite",
                "render",
                "crab.sprite",
                "--png",
                "o.png",
                "--target",
                "own.png",
            ],
            "--target is only for sprite score",
        ),
        (
            &["sprite", "fill"],
            "unknown sprite command \"fill\"; use `render`, `score` or `fit`",
        ),
    ] {
        let (code, out, err) = wmc_in(&dir, &[], args);
        assert!(
            code == Some(1) && err.contains(want),
            "{args:?}: {out}{err}"
        );
    }
    std::fs::remove_dir_all(&dir).unwrap();
}

/// `sprite fit` drafts a file that parses, scores what it says and comes
/// out the same for the same image and seed.
#[test]
fn sprite_fit_drafts_a_sprite_from_an_image() {
    use app::sprite::{parse, raster, sheet};
    let dir = odd_fixtures("sprite-fit");
    let text = std::fs::read_to_string(dir.join("crab.sprite")).unwrap();
    let f = parse("crab.sprite", &text).unwrap();
    let own = raster::draw(&f.sprites[0].poses[0], 0, &[0xd9623b], 0, (1, 1), 48);
    sheet::write_png(&own, &dir.join("own.png")).unwrap();
    let args = [
        "sprite", "fit", "own.png", "--kind", "crab", "-o", "f.sprite", "--stamps", "3",
    ];
    let (code, out, err) = wmc_in(&dir, &[], &args);
    assert_eq!(code, Some(0), "{out}{err}");
    assert!(out.contains("stamp  1: value"), "{out}");
    assert!(out.contains("wrote f.sprite"), "{out}");
    let drafted = std::fs::read_to_string(dir.join("f.sprite")).unwrap();
    assert!(
        drafted.starts_with("# crab: drafted by wmc sprite fit from own.png (seed 1, 1 x 1 cells)"),
        "{drafted}"
    );
    let file = parse("f.sprite", &drafted).unwrap();
    assert_eq!(file.sprites[0].name, "crab");
    assert!((1..=3).contains(&file.sprites[0].poses[0].stamps.len()));
    let (_, again, _) = wmc_in(&dir, &[], &args);
    assert_eq!(again, out);
    assert_eq!(
        std::fs::read_to_string(dir.join("f.sprite")).unwrap(),
        drafted
    );
    // Its score is the one fit printed.
    let shape = out
        .split("shape ")
        .nth(1)
        .unwrap()
        .split(',')
        .next()
        .unwrap();
    let (_, scored, _) = wmc_in(
        &dir,
        &[],
        &["sprite", "score", "f.sprite", "--target", "own.png"],
    );
    assert!(
        scored.contains(&format!("shape   {shape}")),
        "{shape}\n{scored}"
    );
    // The kind's size from the rules; a kind they lack is an error.
    let rules = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../rules");
    let r = rules.to_str().unwrap();
    for (args, want) in [
        (
            &["sprite", "fit", "own.png", "-o", "f.sprite"][..],
            "sprite fit needs --kind NAME",
        ),
        (
            &["sprite", "fit", "own.png", "--kind", "crab"],
            "sprite fit needs -o <out.sprite>",
        ),
        (
            &[
                "sprite", "fit", "own.png", "--kind", "eel", "-o", "f.sprite", "--rules", r,
            ],
            "--kind eel: the rules have no such kind",
        ),
        (
            &[
                "sprite", "fit", "own.png", "--kind", "crab", "-o", "f.sprite", "--stamps", "33",
            ],
            "--stamps needs a count from 1 to 32",
        ),
        (
            &[
                "sprite", "fit", "own.png", "--kind", "crab", "-o", "f.sprite", "--size", "2",
            ],
            "--size needs two values: W H",
        ),
        (
            &[
                "sprite",
                "render",
                "crab.sprite",
                "--png",
                "o.png",
                "--seed",
                "3",
            ],
            "--seed is only for sprite fit",
        ),
        (&["show", "-o", "x"], "only for sprite"),
    ] {
        let (code, out, err) = wmc_in(&dir, &[], args);
        assert!(
            code == Some(1) && err.contains(want),
            "{args:?}: {out}{err}"
        );
    }
    std::fs::remove_dir_all(&dir).unwrap();
}
