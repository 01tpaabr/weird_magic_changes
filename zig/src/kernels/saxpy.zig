//! Example SIMD kernel. Pattern to copy for new kernels:
//!   1. Operate on slices, never raw pointers (root.zig converts at the boundary).
//!   2. Vector body over `@Vector(lanes, T)`, scalar tail.
//!   3. Keep reduction order fixed so results are deterministic.
//!   4. Unit test against a naive scalar reference implementation.
const std = @import("std");

/// Lanes chosen to fit 128-bit NEON / SSE registers; the compiler will pair
/// them up for wider ISAs when `-Dcpu=native` or target-cpu=native is set.
const lanes = std.simd.suggestVectorLength(f32) orelse 4;
const V = @Vector(lanes, f32);

pub fn saxpy(a: f32, x: []const f32, y: []f32) void {
    std.debug.assert(x.len == y.len);
    const av: V = @splat(a);
    var i: usize = 0;
    while (i + lanes <= x.len) : (i += lanes) {
        const xv: V = x[i..][0..lanes].*;
        const yv: V = y[i..][0..lanes].*;
        y[i..][0..lanes].* = av * xv + yv;
    }
    while (i < x.len) : (i += 1) {
        y[i] = a * x[i] + y[i];
    }
}

pub fn sum(x: []const f32) f32 {
    var acc: V = @splat(0);
    var i: usize = 0;
    while (i + lanes <= x.len) : (i += lanes) {
        acc += @as(V, x[i..][0..lanes].*);
    }
    var total: f32 = @reduce(.Add, acc);
    while (i < x.len) : (i += 1) {
        total += x[i];
    }
    return total;
}

// ---------------------------------------------------------------------------
// tests

fn saxpyRef(a: f32, x: []const f32, y: []f32) void {
    for (x, y) |xi, *yi| yi.* = a * xi + yi.*;
}

test "saxpy matches scalar reference, including tail" {
    const n = lanes * 3 + 2; // force a non-multiple-of-lanes tail
    var x: [n]f32 = undefined;
    var y1: [n]f32 = undefined;
    var y2: [n]f32 = undefined;
    for (0..n) |i| {
        x[i] = @floatFromInt(i);
        y1[i] = 1.5;
        y2[i] = 1.5;
    }
    saxpy(2.0, &x, &y1);
    saxpyRef(2.0, &x, &y2);
    for (y1, y2) |a, b| try std.testing.expectEqual(a, b);
}

test "sum" {
    const n = lanes * 2 + 3;
    var x: [n]f32 = undefined;
    for (0..n) |i| x[i] = 1.0;
    try std.testing.expectApproxEqAbs(@as(f32, @floatFromInt(n)), sum(&x), 1e-5);
}
