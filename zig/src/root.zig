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
pub const blit = @import("kernels/blit.zig");

/// ABI version. Bump when an exported signature changes; Rust checks it at startup.
pub const abi_version: u32 = 2;

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

/// Number of glyph boxes an atlas must hold (printable ASCII). Rust asserts
/// its atlas against this before calling `wmc_blit_cells`.
export fn wmc_atlas_glyphs() usize {
    return blit.atlas_glyphs;
}

/// Paint `cols x rows` cells into a 4-bytes-per-pixel buffer. See `kernels/blit.zig` for
/// the layout contract. Pure and deterministic: output depends only on inputs.
/// Buffers must not overlap. Caller partitions `out` into bands of whole cell
/// rows for parallelism.
export fn wmc_blit_cells(
    glyph: [*]const u8,
    fg: [*]const u32,
    bg: [*]const u32,
    cols: usize,
    rows: usize,
    atlas: [*]const u8,
    cell: usize,
    out: [*]u8,
    stride_px: usize,
) void {
    const n = cols * rows;
    blit.blitCells(
        glyph[0..n],
        fg[0..n],
        bg[0..n],
        cols,
        rows,
        atlas[0 .. blit.atlas_glyphs * cell * cell],
        cell,
        out[0 .. rows * cell * stride_px * 4],
        stride_px,
    );
}

test {
    // Pull in tests from every kernel file.
    std.testing.refAllDecls(@This());
}
