//! Scent: per-cell channels that actors raise with `mark` and read with
//! `scent(ch)` and `sniff` (`docs/ACTORS.md` §3). Each channel is a `u8`
//! per cell in [`ChunkCells::scent`]; this module fades them.
//!
//! A chunk's scent fades every [`SCENT_CADENCE`] ticks, staggered by a hash
//! of its coordinate so each tick touches a sixteenth of the chunks. A fade
//! step takes `ceil(s / 32)`: a fresh 255 halves in 21 steps (336 ticks,
//! ~22 game minutes) and is gone in 86 (1376 ticks, ~1.5 game hours). Pure
//! per cell, own chunk only: exact in any order.

use bevy_ecs::prelude::*;

use super::{CHUNK_CELLS, ChunkCells, ChunkCoord, ChunkMeta, SCENT_CHANNELS};
use crate::rng::splitmix64;
use crate::sim::Tick;

/// Ticks between two fade steps of one chunk.
pub const SCENT_CADENCE: u64 = 16;

/// One fade step: `s - ceil(s / 32)`.
#[inline]
pub const fn fade(s: u8) -> u8 {
    s - ((s as u16 + 31) >> 5) as u8
}

/// Does a channel hold any scent? An OR over the whole channel: it
/// vectorises, where an early-exit `any` walks the bytes one by one (that
/// cost +4-11% of a tick once chunks had four channels).
#[inline]
fn scented(ch: &[u8; CHUNK_CELLS]) -> bool {
    ch.iter().fold(0, |a, &s| a | s) != 0
}

/// Is the chunk at `c` due at `tick` for a system of this cadence (a power
/// of two)? Staggered by a hash of the coordinate, never by entity or slot.
#[inline]
pub fn chunk_due(tick: u64, c: ChunkCoord, cadence: u64) -> bool {
    let stagger = splitmix64(u64::from(c.x as u32) << 32 | u64::from(c.y as u32));
    tick.wrapping_add(stagger) & (cadence - 1) == 0
}

/// The Simulate phase: fade the scent of every due chunk. A chunk whose
/// scent changed is dirty (it must be saved to be seen again). One thread:
/// a sixteenth of the chunks per tick is too little work to pay for
/// parallel dispatch (`par_iter_mut` measured 5% slower on the whole tick).
/// Only channels holding scent are touched: most rule sets use one or two.
pub fn scent_decay(tick: Res<Tick>, mut q: Query<(&ChunkCoord, &mut ChunkCells, &mut ChunkMeta)>) {
    let tick = tick.0;
    for (c, mut cells, mut meta) in &mut q {
        if !chunk_due(tick, *c, SCENT_CADENCE) {
            continue;
        }
        let live: [bool; SCENT_CHANNELS] = std::array::from_fn(|j| scented(&cells.scent[j]));
        if !live.contains(&true) {
            continue;
        }
        for (ch, live) in cells.scent.iter_mut().zip(live) {
            if live {
                for s in ch.iter_mut() {
                    *s = fade(*s);
                }
            }
        }
        meta.dirty = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fading_takes_a_32nd_rounded_up_and_ends_at_zero() {
        assert_eq!(fade(0), 0);
        assert_eq!(fade(1), 0);
        assert_eq!(fade(32), 31);
        assert_eq!(fade(33), 31);
        assert_eq!(fade(255), 247);
        let (mut s, mut steps, mut half) = (255u8, 0, 0);
        while s > 0 {
            s = fade(s);
            steps += 1;
            if s <= 127 && half == 0 {
                half = steps;
            }
        }
        assert_eq!((half, steps), (21, 86));
    }

    #[test]
    fn chunks_are_due_once_per_cadence_and_staggered() {
        let c = ChunkCoord::new(3, -2);
        let due: Vec<u64> = (0..64).filter(|&t| chunk_due(t, c, 16)).collect();
        assert_eq!(due.len(), 4);
        assert!(due.windows(2).all(|w| w[1] - w[0] == 16));
        let firsts: std::collections::BTreeSet<u64> = (0..32)
            .map(|x| {
                (0..16)
                    .find(|&t| chunk_due(t, ChunkCoord::new(x, 0), 16))
                    .unwrap()
            })
            .collect();
        assert!(firsts.len() > 8, "stagger spreads chunks: {firsts:?}");
    }
}
