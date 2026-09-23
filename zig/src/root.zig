//! Root of the Zig kernel library.
//!
//! Everything exported to Rust lives behind a C ABI (`export fn`) and is
//! prefixed `wmc_`. Rules for the boundary:
//!   * Only C-compatible types cross: integers, floats, `[*]T` pointers + `usize` len,
//!     `extern struct`s. Never slices, optionals, error unions, or Zig-only structs.
//!   * No allocation across the boundary. The caller (Rust) owns all memory.
//!   * Kernels are pure functions over buffers. No global state, no threads.
//!   * Every export gets a `test` below it and a matching safe wrapper in
//!     `crates/zig-kernels/src/lib.rs`.
const std = @import("std");

pub const saxpy = @import("kernels/saxpy.zig");

/// ABI version. Bump when an exported signature changes; Rust checks it at startup.
pub const abi_version: u32 = 1;

export fn wmc_abi_version() u32 {
    return abi_version;
}

/// y[i] = a * x[i] + y[i] for i in 0..n
export fn wmc_saxpy_f32(a: f32, x: [*]const f32, y: [*]f32, n: usize) void {
    saxpy.saxpy(a, x[0..n], y[0..n]);
}

/// Sum of x[0..n]. Deterministic (fixed reduction order) so results are
/// reproducible across runs; do not "optimize" into a non-associative fast path.
export fn wmc_sum_f32(x: [*]const f32, n: usize) f32 {
    return saxpy.sum(x[0..n]);
}

test {
    // Pull in tests from every kernel file.
    std.testing.refAllDecls(@This());
}
