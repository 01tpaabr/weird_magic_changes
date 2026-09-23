//! Entry point. Currently a headless smoke run; rendering/input come later.
use std::time::Instant;

use anyhow::{Context, bail};
use sim_core::World;

fn main() -> anyhow::Result<()> {
    if let Err(v) = zig_kernels::check_abi() {
        bail!(
            "Zig kernel ABI mismatch: rust={} zig={v}",
            zig_kernels::ABI_VERSION
        );
    }

    let n: usize = std::env::args()
        .nth(1)
        .map_or(Ok(1_000_000), |s| s.parse())
        .context("entity count")?;
    let steps: u64 = std::env::args()
        .nth(2)
        .map_or(Ok(600), |s| s.parse())
        .context("step count")?;

    let mut world = World::new(n, 42);
    let t0 = Instant::now();
    for _ in 0..steps {
        world.step(1.0 / 60.0);
    }
    let dt = t0.elapsed();

    println!("entities:   {n}");
    println!("steps:      {steps}");
    println!("threads:    {}", rayon_threads());
    println!("total:      {dt:.2?}");
    println!(
        "per step:   {:.2?}",
        dt / u32::try_from(steps).unwrap_or(u32::MAX)
    );
    println!("checksum:   {:.6}", world.checksum());
    Ok(())
}

fn rayon_threads() -> usize {
    std::thread::available_parallelism().map_or(1, |n| n.get())
}
