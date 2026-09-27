//! Derived randomness. There is no global RNG anywhere in the sim, and no
//! stream with state: every draw is a pure function of what names it, so
//! it does not depend on chunking, thread count, or evaluation order.
//!
//! - [`hash_cell`]: a pure hash of `(seed, stream, x, y)`, for a value per
//!   cell (worldgen, placement).
//! - Counter-based draws: a base hashed from what the unit of work is (the
//!   actor `uid` and the tick: `rules::vm::rng_base`), and draw `n` is
//!   `splitmix64(base + n)` (the VM's `rand` and `chance`).
//!
//! `stream` constants let different uses of the same `(seed, x, y)` stay
//! uncorrelated. Add new ones as `pub const STREAM_*: u64` next to the system
//! that uses them; never reuse a number.

/// splitmix64 finaliser: cheap, good avalanche, the standard way to turn a
/// counter into a seed.
#[inline]
pub fn splitmix64(mut z: u64) -> u64 {
    z = z.wrapping_add(0x9E37_79B9_7F4A_7C15);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

/// Pure hash of a (signed) cell coordinate for one named `stream`.
#[inline]
pub fn hash_cell(seed: u64, stream: u64, x: i32, y: i32) -> u64 {
    // Bit patterns, so negative coords hash as well as positive ones.
    let xy = (u64::from(x as u32) << 32) | u64::from(y as u32);
    splitmix64(seed ^ splitmix64(stream) ^ splitmix64(xy))
}

/// Map a 64-bit hash to a float in `[0, 1)` using the top 24 bits (exact in f32).
#[inline]
pub fn unit_f32(h: u64) -> f32 {
    (h >> 40) as f32 / (1u64 << 24) as f32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_cell_is_pure_and_stream_separated() {
        assert_eq!(hash_cell(1, 0, 3, 4), hash_cell(1, 0, 3, 4));
        assert_ne!(hash_cell(1, 0, 3, 4), hash_cell(1, 1, 3, 4));
        assert_ne!(hash_cell(1, 0, 3, 4), hash_cell(1, 0, 4, 3));
        assert_ne!(hash_cell(1, 0, 3, 4), hash_cell(2, 0, 3, 4));
        assert_ne!(hash_cell(1, 0, -3, 4), hash_cell(1, 0, 3, 4));
    }

    #[test]
    fn unit_f32_in_range() {
        for i in -5_000..5_000i32 {
            let f = unit_f32(hash_cell(9, 9, i, 0));
            assert!((0.0..1.0).contains(&f));
        }
    }
}
