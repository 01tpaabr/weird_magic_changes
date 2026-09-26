//! Every scenario in `scenarios/` (its tests in `tests/` too), on the real
//! binary: each must pass its `expect` lines, and end with the same checksum
//! on 1 thread and on 8, so every one is a determinism check too. A scenario
//! runs with the packs it names (`rules`), else the built-in rules.

use std::path::Path;
use std::process::Command;

fn run(file: &Path, threads: &str) -> String {
    let out = Command::new(env!("CARGO_BIN_EXE_wmc"))
        .env_remove("WMC_RULES")
        .args(["scenario", file.to_str().unwrap(), "--threads", threads])
        .output()
        .expect("wmc runs");
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(
        out.status.success(),
        "{} at {threads} threads:\n{stdout}{}",
        file.display(),
        String::from_utf8_lossy(&out.stderr)
    );
    stdout
}

fn checksum(report: &str) -> &str {
    report
        .lines()
        .find(|l| l.starts_with("checksum:"))
        .unwrap_or_else(|| panic!("no checksum in:\n{report}"))
}

#[test]
fn every_scenario_passes_at_1_and_8_threads() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../scenarios");
    let mut files = Vec::new();
    for dir in [root.clone(), root.join("tests")] {
        for e in std::fs::read_dir(&dir).unwrap() {
            let p = e.unwrap().path();
            if p.extension().is_some_and(|x| x == "scenario") {
                files.push(p);
            }
        }
    }
    files.sort();
    assert!(files.len() >= 7, "{files:?}");
    for f in &files {
        let (one, eight) = (run(f, "1"), run(f, "8"));
        assert!(one.contains("(1 threads)") && eight.contains("(8 threads)"));
        assert_eq!(checksum(&one), checksum(&eight), "{}", f.display());
    }
}

/// A failing expectation says what it found and fails the run.
#[test]
fn a_failed_expectation_is_reported_and_fails() {
    let dir = std::env::temp_dir().join(format!("wmc-scenario-fail-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let f = dir.join("wrong.scenario");
    std::fs::write(
        &f,
        "size 8 8\nstart egg at (3, 3)\nrun 1\nexpect count egg == 2\nexpect at (3, 3) egg\nexpect count wolf == 0\n",
    )
    .unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_wmc"))
        .env_remove("WMC_RULES")
        .args(["scenario", f.to_str().unwrap()])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(!out.status.success());
    assert!(stdout.contains("expect count egg == 2  (1)"), "{stdout}");
    assert!(
        stdout.contains("ok    ") && stdout.contains("expect at (3, 3) egg  (egg)"),
        "{stdout}"
    );
    assert!(stdout.contains("the rules have no kind `wolf`"), "{stdout}");
    assert!(String::from_utf8_lossy(&out.stderr).contains("2 of 3 expectations failed"));
    std::fs::remove_dir_all(&dir).unwrap();
}
