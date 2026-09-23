//! The whole simulation state and its tick.
//!
//! A tick is a fixed sequence of **phases**. Inside a phase, work is split into
//! independent chunks with no shared mutable state; between phases there is a
//! barrier. Systems live in their own modules and are plain functions of the
//! form `fn(&Stage /*read*/, &mut Layer /*write, chunked*/)`; `World::step`
//! is the only place that decides their order. Cross-entity effects go through
//! double buffers or per-chunk accumulation + in-order merge (see
//! `/parallel-sim`).

use crate::stage::{
    Stage,
    worldgen::{GenParams, generate},
};

#[derive(Debug, Clone)]
pub struct World {
    pub seed: u64,
    pub tick: u64,
    pub stage: Stage,
}

impl World {
    /// Generate a fresh world. Same arguments => bit-identical world.
    pub fn generate(width: u32, height: u32, seed: u64, params: &GenParams) -> Self {
        Self {
            seed,
            tick: 0,
            stage: generate(width, height, seed, params),
        }
    }

    /// Advance one tick. Phases are listed here, in order, and nowhere else.
    pub fn step(&mut self) {
        // phase 1..n: (no systems yet; the stage is static until actors land)
        self.tick += 1;
    }

    /// Checksum of all state, for determinism tests and bug reports.
    pub fn checksum(&self) -> u64 {
        crate::rng::splitmix64(self.stage.checksum() ^ self.tick)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn step_advances_tick_and_changes_checksum() {
        let mut w = World::generate(32, 32, 5, &GenParams::default());
        let c0 = w.checksum();
        w.step();
        assert_eq!(w.tick, 1);
        assert_ne!(w.checksum(), c0);
    }
}
