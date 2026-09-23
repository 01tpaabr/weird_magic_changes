# Zig 0.16 cheatsheet (things that changed since 0.11–0.14)

Source of truth: `/opt/homebrew/Cellar/zig/0.16.0_1/lib/zig/std` (or `zig env`).
`grep -rn "pub fn name" $STD` beats memory every time.

## Build system (`build.zig`)

```zig
const target = b.standardTargetOptions(.{});
const optimize = b.standardOptimizeOption(.{});
const mod = b.addModule("name", .{ .root_source_file = b.path("src/root.zig"), .target = target, .optimize = optimize, .link_libc = false });
const lib = b.addLibrary(.{ .linkage = .static, .name = "name", .root_module = mod });
b.installArtifact(lib);
const tests = b.addTest(.{ .root_module = mod });
b.step("test", "Run tests").dependOn(&b.addRunArtifact(tests).step);
```
- `addStaticLibrary`/`addSharedLibrary` are gone; use `addLibrary` with `.linkage`.
- Executables/libs take a `root_module`; create with `b.createModule` (private) or
  `b.addModule` (exported).
- `build.zig.zon` needs `.fingerprint`; if wrong, the compiler prints the right value.
- Flags: `-Doptimize=ReleaseFast`, `-Dcpu=native`, `--prefix DIR`, `--cache-dir DIR`.

## Language

- Calling convention: `callconv(.c)` (lowercase). `export fn` implies C ABI.
- `for (a, b, 0..) |x, y, i| {}` multi-object loops; lengths must be equal.
- Labeled switch continue: `sw: switch (x) { .a => continue :sw .b, .b => {} }`.
- `@splat(x)` infers the vector type from context; `@reduce(.Add, v)`.
- Slice-to-array-pointer: `x[i..][0..N].*` gives `[N]T` by value; assign back the same way.
- `@intFromFloat`, `@floatFromInt`, `@intCast`, `@truncate`, `@as(T, v)`. Result-location
  inference means `const v: V = @splat(1.0);` works.
- `usingnamespace` removed. `async`/`await` not available.
- `std.builtin.Type` field names are lowercase (`.@"struct"`, `.int`, `.pointer`).
- `@This()`, `comptime`, `inline for`, `anytype` unchanged.

## std

- `std.ArrayList(T)` is unmanaged: `var l: std.ArrayList(T) = .empty; try l.append(gpa, v); l.deinit(gpa);`
  Managed variant: `std.array_list.Managed(T)` (deprecated; avoid).
- `std.heap.DebugAllocator(.{})` (was GeneralPurposeAllocator), `std.heap.smp_allocator`,
  `std.heap.page_allocator`, `std.heap.ArenaAllocator`, `std.heap.FixedBufferAllocator`.
- I/O moved to `std.Io`: `std.Io.Writer`, `std.Io.Reader` with explicit buffers and `flush()`.
  Templates show `std.fs.File.stdout().writer(&buf)` style; verify in std before using.
  Kernels don't do I/O; `std.debug.print` still works for debugging.
- Threads: `std.Thread.spawn(.{}, f, .{args})`, `std.Thread.Pool`, `std.Thread.ResetEvent`,
  `std.atomic.Value(T)` with `.load(.acquire)` / `.store(v, .release)` / `.fetchAdd`.
  (We don't thread in Zig in this repo; rayon does it.)
- SIMD helpers: `std.simd.suggestVectorLength(T)`, `std.simd.iota`, `std.simd.join`.
- Testing: `std.testing.expect`, `expectEqual(expected, actual)`, `expectApproxEqAbs`,
  `expectEqualSlices(T, a, b)`, `std.testing.allocator` (leak-checked),
  `std.testing.refAllDecls(@This())`.
- `std.mem`: `copyForwards`, `@memcpy(dst, src)`, `@memset(dst, v)`, `std.mem.eql`.
- `std.math`: `clamp`, `lerp`, `sqrt` is `@sqrt` builtin; `std.math.floatEps`.

## Running

```
zig build            # default step: install artifacts to zig-out/
zig build test       # run tests
zig build test -Dtest-filter="name"   # if wired; else `zig test src/file.zig`
zig test src/kernels/saxpy.zig        # quickest single-file loop
zig fmt --check .    # CI; `zig fmt .` to fix
zig run file.zig     # scratch experiments
```
