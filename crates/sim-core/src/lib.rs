//! Simulation core.
//!
//! Shape of everything in here: **data in flat arrays, systems as pure
//! functions over slices, parallelism at the chunk level, determinism by
//! construction.** See `docs/ARCHITECTURE.md` for the decisions and
//! `/parallel-sim` for the patterns.
//!
//! Modules:
//! - [`stage`]: the chunked, unbounded 2D grid every actor stands on.
//! - [`rng`]: derived, shared-nothing randomness (`hash_cell`, `rng_for`).
//! - [`store`]: save directory format (meta + per-chunk files).
//! - [`time`]: the integer clock: ticks per day, calendar, daylight.
//! - [`world`]: seed + tick + stage (+ actors, later), streaming, the phase
//!   sequence.

pub mod rng;
pub mod stage;
pub mod store;
pub mod time;
pub mod world;

pub use stage::{
    ActorId, CHUNK_BITS, CHUNK_CELLS, CHUNK_SIZE, Cell, ChunkCells, ChunkCoord, Feature, Ground,
    Pos, Stage,
};
pub use store::Store;
pub use time::{Clock, TICKS_PER_DAY, daylight};
pub use world::{LoadPolicy, StreamStats, World, WorldConfig};
