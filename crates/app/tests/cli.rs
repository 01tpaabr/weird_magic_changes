//! The CLI on the real binary: what `wmc why` reports.

use std::path::PathBuf;
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

/// A fresh scratch directory for one test.
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
}
