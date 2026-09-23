//! Build script for the Zig kernel library.
//!
//! Produces `libwmc_kernels.a`, a static library with a C ABI that the Rust
//! `zig-kernels` crate links against. Cargo drives this file through
//! `crates/zig-kernels/build.rs`; you can also run it by hand:
//!
//!   cd zig && zig build            # -> zig-out/lib/libwmc_kernels.a
//!   cd zig && zig build test       # run Zig unit tests
//!
//! Keep this file small. Add new kernels as files under src/kernels/ and
//! re-export their `export fn`s from src/root.zig.
const std = @import("std");

pub fn build(b: *std.Build) void {
    const target = b.standardTargetOptions(.{});
    const optimize = b.standardOptimizeOption(.{});

    const mod = b.addModule("wmc_kernels", .{
        .root_source_file = b.path("src/root.zig"),
        .target = target,
        .optimize = optimize,
        // Kernels are pure compute: no libc, no allocator, no threads.
        // Threading is Rust's job (rayon). Flip this if a kernel truly needs libc.
        .link_libc = false,
    });

    const lib = b.addLibrary(.{
        .linkage = .static,
        .name = "wmc_kernels",
        .root_module = mod,
    });
    b.installArtifact(lib);

    // `zig build test`
    const tests = b.addTest(.{ .root_module = mod });
    const run_tests = b.addRunArtifact(tests);
    const test_step = b.step("test", "Run Zig unit tests");
    test_step.dependOn(&run_tests.step);
}
