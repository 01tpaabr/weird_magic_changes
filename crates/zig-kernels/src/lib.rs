//! Safe wrappers over the Zig kernels in `zig/src/root.zig`.
//!
//! Every `extern "C"` declaration here must match an `export fn` in Zig
//! **exactly** (name, argument order, types). The ABI version check in
//! [`check_abi`] catches stale builds but not signature drift, so when you change
//! a signature, change both sides in the same commit and add a test.
//!
//! Layering rule: this crate is the *only* place in the workspace allowed to
//! write `unsafe extern "C"`. Everything above it sees plain safe Rust.

/// ABI version this crate was written against. Must equal `abi_version` in `zig/src/root.zig`.
pub const ABI_VERSION: u32 = 1;

mod ffi {
    unsafe extern "C" {
        pub fn wmc_abi_version() -> u32;
        pub fn wmc_saxpy_f32(a: f32, x: *const f32, y: *mut f32, n: usize);
        pub fn wmc_sum_f32(x: *const f32, n: usize) -> f32;
    }
}

/// Returns `Err` with the Zig-side version if the linked library does not match [`ABI_VERSION`].
pub fn check_abi() -> Result<(), u32> {
    // SAFETY: no arguments, no side effects.
    let v = unsafe { ffi::wmc_abi_version() };
    if v == ABI_VERSION { Ok(()) } else { Err(v) }
}

/// `y[i] = a * x[i] + y[i]`. Panics if lengths differ.
#[inline]
pub fn saxpy(a: f32, x: &[f32], y: &mut [f32]) {
    assert_eq!(x.len(), y.len(), "saxpy: length mismatch");
    // SAFETY: both slices are valid for `len` elements and `y` is uniquely borrowed.
    unsafe { ffi::wmc_saxpy_f32(a, x.as_ptr(), y.as_mut_ptr(), x.len()) }
}

/// Deterministic sum of `x`.
#[inline]
pub fn sum(x: &[f32]) -> f32 {
    // SAFETY: slice is valid for `len` elements; the kernel only reads.
    unsafe { ffi::wmc_sum_f32(x.as_ptr(), x.len()) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn abi_matches() {
        check_abi().expect("Zig ABI version mismatch: rebuild or bump ABI_VERSION on both sides");
    }

    #[test]
    fn saxpy_matches_reference() {
        let n = 37; // non-multiple of any SIMD width
        let x: Vec<f32> = (0..n).map(|i| i as f32).collect();
        let mut y = vec![1.5f32; n];
        let mut expected = y.clone();
        saxpy(2.0, &x, &mut y);
        for (e, xi) in expected.iter_mut().zip(&x) {
            *e += 2.0 * xi;
        }
        assert_eq!(y, expected);
    }

    #[test]
    fn sum_is_deterministic() {
        let x: Vec<f32> = (0..1000).map(|i| (i as f32).sin()).collect();
        assert_eq!(sum(&x).to_bits(), sum(&x).to_bits());
        assert!((sum(&x) - x.iter().sum::<f32>()).abs() < 1e-2);
    }

    #[test]
    fn empty_slices() {
        assert_eq!(sum(&[]), 0.0);
        saxpy(1.0, &[], &mut []);
    }
}
