//! Simulation core.
//!
//! Shape of everything in here: **data in flat arrays, systems as pure
//! functions over slices, parallelism at the chunk level, determinism by
//! construction.** See `docs/ARCHITECTURE.md` for the decisions and
//! `/parallel-sim` for the patterns.
//!
//! Modules:
//! - [`stage`]: the 2D grid every actor stands on (ground, features, occupancy).
//! - [`rng`]: derived, shared-nothing randomness (`hash_cell`, `rng_for`).
//! - [`world`]: seed + tick + stage (+ actors, later) and the phase sequence.

pub mod rng;
pub mod stage;
pub mod world;

pub use stage::{ActorId, CellIdx, Feature, Ground, Pos, Stage};
pub use world::World;
