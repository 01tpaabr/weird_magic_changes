//! Builds `zig/` with `zig build` and links the resulting static library.
//!
//! Optimize mode follows cargo's opt-level so `cargo build` gives you
//! bounds-checked Zig (ReleaseSafe) and `cargo build --release` gives ReleaseFast.
//! Override with `WMC_ZIG_OPTIMIZE=Debug|ReleaseSafe|ReleaseFast|ReleaseSmall`.
use std::env;
use std::path::PathBuf;
use std::process::Command;

fn main() {
    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let zig_dir = manifest_dir
        .join("../../zig")
        .canonicalize()
        .expect("zig/ directory missing");
    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());

    // Re-run when any Zig source changes.
    println!(
        "cargo:rerun-if-changed={}",
        zig_dir.join("build.zig").display()
    );
    println!(
        "cargo:rerun-if-changed={}",
        zig_dir.join("build.zig.zon").display()
    );
    println!("cargo:rerun-if-changed={}", zig_dir.join("src").display());
    println!("cargo:rerun-if-env-changed=WMC_ZIG_OPTIMIZE");
    println!("cargo:rerun-if-env-changed=ZIG");

    let optimize = env::var("WMC_ZIG_OPTIMIZE").unwrap_or_else(|_| {
        match env::var("OPT_LEVEL").as_deref() {
            Ok("0") => "Debug",
            Ok("1") => "ReleaseSafe",
            _ => "ReleaseFast",
        }
        .to_string()
    });

    let zig = env::var("ZIG").unwrap_or_else(|_| "zig".to_string());
    let prefix = out_dir.join("zig");
    let cache = out_dir.join("zig-cache");

    let status = Command::new(&zig)
        .current_dir(&zig_dir)
        .args(["build", "--prefix"])
        .arg(&prefix)
        .arg("--cache-dir")
        .arg(&cache)
        .arg(format!("-Doptimize={optimize}"))
        // Match cargo's target-cpu=native so both halves use the same ISA.
        .arg("-Dcpu=native")
        .status()
        .unwrap_or_else(|e| panic!("failed to run `{zig} build` (is zig on PATH? run `make setup`): {e}"));
    assert!(status.success(), "`zig build` failed (see output above)");

    println!(
        "cargo:rustc-link-search=native={}",
        prefix.join("lib").display()
    );
    println!("cargo:rustc-link-lib=static=wmc_kernels");
}
