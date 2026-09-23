//! The determinism gate, on the real binary: Bevy's compute pool is one per
//! process, so thread counts are compared across processes. Same seed, same
//! ticks, same checksum with 1, 3 and 8 threads; same map text from `show`.

use std::path::PathBuf;
use std::process::Command;

fn wmc(threads: &str, args: &[&str]) -> String {
    let out = Command::new(env!("CARGO_BIN_EXE_wmc"))
        .env("WMC_THREADS", threads)
        .args(args)
        .output()
        .expect("wmc runs");
    assert!(
        out.status.success(),
        "wmc {args:?} failed:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).expect("utf8 output")
}

fn line(output: &str, key: &str) -> String {
    output
        .lines()
        .find(|l| l.starts_with(key))
        .unwrap_or_else(|| panic!("no {key} line in:\n{output}"))
        .to_string()
}

fn tmp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("wmc-det-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

#[test]
fn run_checksum_is_identical_across_thread_counts() {
    let dir = tmp_dir("run");
    let dir = dir.to_str().unwrap();
    // `run` on a directory without a save creates the world and never writes.
    let args = ["run", dir, "300", "300", "200", "7"];
    let one = wmc("1", &args);
    let three = wmc("3", &args);
    let eight = wmc("8", &args);
    assert!(line(&one, "wall:").contains("1 threads"), "{one}");
    assert!(line(&eight, "wall:").contains("8 threads"), "{eight}");
    assert_eq!(line(&one, "checksum:"), line(&three, "checksum:"));
    assert_eq!(line(&one, "checksum:"), line(&eight, "checksum:"));
    assert!(line(&one, "ticks:").contains("300 ("));
    // A different tick count is a different world state.
    let fewer = wmc("8", &["run", dir, "299", "300", "200", "7"]);
    assert_ne!(line(&one, "checksum:"), line(&fewer, "checksum:"));
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn show_is_identical_across_thread_counts() {
    let args = ["show", "200", "100", "5"];
    let one = wmc("1", &args);
    let eight = wmc("8", &args);
    let map = |s: &str| {
        s.lines()
            .take_while(|l| !l.starts_with("stage:"))
            .map(str::to_string)
            .collect::<Vec<_>>()
    };
    assert_eq!(map(&one), map(&eight));
    assert_eq!(map(&one).len(), 100 + 1);
    assert_eq!(line(&one, "checksum:"), line(&eight, "checksum:"));
    assert_ne!(
        line(&one, "checksum:"),
        line(&wmc("1", &["show", "200", "100", "6"]), "checksum:")
    );
}
