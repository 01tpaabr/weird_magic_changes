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
    let (ok, _, err) = wmc(&["show", "20", "5", "42", "--seed", "7"]);
    assert!(!ok && err.contains("unknown flag --seed"), "{err}");
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
    // What they do use still works.
    let (ok, out, err) = wmc(&["scenario", fox_pen, "--threads", "1"]);
    assert!(ok, "{out}{err}");
    let rules = repo.join("rules");
    let (ok, out, err) = wmc(&["lint", rules.to_str().unwrap(), "--strict"]);
    assert!(ok, "{out}{err}");
    assert!(!Path::new(dir).exists(), "run never saves");
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
    let cases: &[&[&str]] = &[
        &[],
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

/// What a generated command line is made of: no `play` (a window), and no
/// number a tick count reads as long.
const WORDS: &[&str] = &[
    "show",
    "run",
    "why",
    "lint",
    "scenario",
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
