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
//! - [`actors`]: actor rows inside chunks and the Think/Apply/Compact phases.
//! - [`rules`]: the kind table, the rules VM and the built-in programs (`docs/ACTORS.md`).
//! - [`rng`]: derived, shared-nothing randomness (`hash_cell`, `rng_for`).
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
pub mod sim;
pub mod stage;
pub mod store;
pub mod time;

pub use actors::{ActorMind, ActorPub, ChunkActors, ChunkMinds};
pub use rules::{KindDef, Kinds};
pub use sim::{LoadPolicy, Phase, SimConfig, SimTick, StreamStats, Tick, WorldConfig};
pub use stage::{
    ActorId, CHUNK_BITS, CHUNK_CELLS, CHUNK_SIZE, Cell, ChunkCells, ChunkCoord, ChunkData,
    ChunkMeta, Feature, Ground, Pos, SCENT_CHANNELS, Stage, StageCells,
};
pub use store::Store;
pub use time::{Clock, TICKS_PER_DAY, daylight};
