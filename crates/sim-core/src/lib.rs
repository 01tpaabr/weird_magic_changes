//! Simulation core, on `bevy_ecs`.
//!
//! Shape of everything in here: **chunks are entities, a chunk's cells are one
//! component of flat arrays, systems are plain functions over queries,
//! parallelism is `par_iter_mut` over chunks, determinism by construction.**
//! See `docs/ARCHITECTURE.md` for the decisions and `/parallel-sim` +
//! `/bevy-dev` for the patterns.
//!
//! This crate depends on `bevy_ecs` and `bevy_tasks` only: no clock, no
//! assets, no window. The engine-facing `App`, plugins and rendering live in
//! `app`.
//!
//! Modules:
//! - [`stage`]: the chunked, unbounded 2D grid every actor stands on.
//! - [`actors`]: actor rows inside chunks and the actor phases (Think,
//!   Resolve, Exchange, Apply, Migrate, Compact).
//! - [`rules`]: the kind table, the rules VM and the built-in programs (`docs/ACTORS.md`).
//! - [`reload`]: rule swaps and save remapping ([`reload::Plan`]).
//! - [`rng`]: derived, counter-based randomness (`hash_cell`, `splitmix64`).
//! - [`scenario`]: the world a save is made from (seed, size, terrain,
//!   where kinds start), and its resolution against the rules.
//! - [`store`]: save directory format (meta + per-chunk files).
//! - [`time`]: the integer clock: ticks per day, calendar, daylight.
//! - [`sim`]: resources (`SimConfig`, `Tick`), the `SimTick` schedule and its
//!   phases, world creation, streaming, save/load, checksum.
//! - [`par`]: deterministic parallel helpers over the compute task pool.

pub mod actors;
pub mod par;
pub mod reload;
pub mod rng;
pub mod rules;
pub mod scenario;
pub mod sim;
pub mod stage;
pub mod store;
pub mod time;

pub use actors::{ActorMind, ActorPub, ChunkActors, ChunkMinds};
pub use rules::{KindDef, Kinds};
pub use scenario::Scenario;
pub use sim::{LoadPolicy, Phase, SimConfig, SimTick, StreamStats, Tick};
pub use stage::{
    ActorId, CHUNK_BITS, CHUNK_CELLS, CHUNK_SIZE, ChunkCells, ChunkCoord, ChunkData, ChunkMeta,
    Feature, Ground, Pos, SCENT_CHANNELS, Stage, StageCells,
};
pub use store::Store;
pub use time::{Clock, TICKS_PER_DAY, daylight};

/// Proptest settings for the never-panic properties (scenario text, save
/// files): `cases` for `make test`, `WMC_FUZZ_CASES` for `make fuzz`. The
/// seed is fixed, so `make test` checks the same cases every run;
/// `WMC_FUZZ_SEED` picks others (`make fuzz` prints the one it used).
/// Nothing is persisted: a failure prints its shrunk input, which goes in a
/// regression test.
#[cfg(test)]
pub(crate) fn fuzz_config(cases: u32) -> proptest::test_runner::Config {
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
