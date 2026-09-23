---
name: zig-rust-dev
description: Rust + Zig coding practices for this massively parallel game/sim. Loaded automatically at session start. Re-invoke after context compaction or before writing any Rust system, Zig kernel, or FFI code.
---

# zig-rust-dev

You are writing a massively parallel simulation. Read `CLAUDE.md` priorities first; this
skill is the how. Deeper material: `references/zig-0.16-cheatsheet.md`,
`references/ffi-boundary.md`. For parallel *design* patterns invoke `/parallel-sim`.

## Division of labor

| Rust (crates/sim-core, crates/app)        | Zig (zig/src)                                  |
|-------------------------------------------|------------------------------------------------|
| World state, entity ids, systems, phases  | Hot inner loops over flat buffers              |
| Threading (rayon), scheduling, job graph  | Explicit SIMD via `@Vector`                    |
| Allocation, lifetimes, ownership          | Nothing allocated, nothing owned               |
| I/O, rendering, input, config, logging    | No I/O, no globals, no threads, no libc        |
| Tests of behavior + determinism           | Tests vs. scalar reference implementation      |

Default to Rust. Move a loop to Zig only when (a) it is measured hot, (b) it is a pure
function over slices, and (c) `@Vector` or explicit control gives a measured win over
what rustc autovectorizes. A Zig kernel that isn't benchmarked against the Rust version
is a liability, not an optimization.

## Rust practices (perf-critical, edition 2024)

- **Layout:** SoA `Vec<f32>` per attribute; `#[repr(C)]` + `bytemuck::Pod` for anything
  that crosses FFI or goes to GPU; `u32` ids; `Vec<Option<T>>` is a smell, use a free list
  or a dense/sparse set.
- **Hot loops:** iterate slices, not indices with bounds checks in the middle; use
  `chunks_exact`/`zip` so the compiler can elide checks and vectorize; `assert_eq!` lengths
  once at the top of a function, not per element. No `Box<dyn>`/`Rc`/`String` in per-entity
  data. No allocation inside a tick: pre-size buffers, reuse scratch (`Vec::clear()`).
- **Parallelism:** `rayon` `par_chunks_mut` / `par_iter` / `join` / `scope`. Chunk size is
  a named constant, tuned by bench. `std::thread::scope` for long-lived worker threads.
  `crossbeam` channels for cross-phase pipelines. `parking_lot` if a lock is truly needed
  (frame boundaries only). Never `Arc<Mutex<Vec<..>>>` as the world.
- **Determinism:** no `HashMap` iteration in sim logic (use `Vec` or sorted keys);
  seeded `rand_xoshiro::Xoshiro256PlusPlus` derived per chunk/entity from (seed, id, tick);
  fixed-order reductions (`.fold` per chunk then sequential merge, or `sum` over chunk results
  in chunk order); never `f32` reductions via `par_iter().sum()`.
- **Errors:** `anyhow` in `app`, `thiserror` in libraries, panics for invariant violations
  in sim code (a corrupt world should crash loudly, not limp).
- **API:** functions take `&[T]`/`&mut [T]`, not `&Vec<T>`. `#[inline]` on tiny wrappers
  that cross crates; nothing else. No `pub` without a reason.
- **Edition 2024:** `unsafe extern "C" { ... }`; unsafe ops inside `unsafe fn` need an inner
  `unsafe {}` block; every `unsafe {}` has a `// SAFETY:` comment stating the invariant.
- **Lints:** `cargo clippy -D warnings` is the bar. Fix, don't `#[allow]`, unless the allow
  has a one-line justification.

## Zig practices (0.16)

Your training data is mostly Zig 0.11–0.14. 0.15/0.16 broke a lot. When unsure, **grep
the installed std** (`zig env | grep std_dir`), or run `zig init` in scratch and read the
generated files. Never write a std API call from memory if you can check it in 10 seconds.

- **Kernels are pure:** `pub fn kernel(x: []const f32, y: []f32) void`. Slices inside,
  many-pointers `[*]T` + `usize` only at the `export fn` boundary (in `root.zig`).
- **SIMD:** `const V = @Vector(lanes, f32)`, `lanes = std.simd.suggestVectorLength(f32)
  orelse 4`, load with `x[i..][0..lanes].*`, `@splat`, `@reduce(.Add, v)`, scalar tail
  loop. Keep reduction order fixed.
- **Multi-object loops:** `for (x, y) |xi, *yi| yi.* = xi;` (lengths must match; assert).
- **Style:** `zig fmt` is law. `snake_case` fns, `TitleCase` types, `camelCase` for
  function-returning-type is *not* used; `std.debug.assert` for invariants.
- **Containers:** `std.ArrayList(T)` is *unmanaged*: init `.empty`, every mutating method
  takes the allocator (`list.append(gpa, v)`). But kernels shouldn't need containers.
- **Tests:** every kernel file has `test "..."` blocks comparing to a naive scalar
  reference with a length that is *not* a multiple of `lanes`. `root.zig` has
  `test { std.testing.refAllDecls(@This()); }` to pull them in.
- **Allocators (if ever needed outside kernels):** `std.heap.DebugAllocator` in debug,
  `std.heap.smp_allocator` for multithreaded release, `std.heap.page_allocator` for big
  one-shot buffers. Pass `std.mem.Allocator` explicitly; never a global.
- **No:** `usingnamespace` (removed), `async` (not available), `callconv(.C)` (it's `.c`),
  `std.io.getStdOut()` (I/O went through `std.Io` rewrite; kernels don't print anyway).

## FFI boundary (the only unsafe in the repo)

Full rules in `references/ffi-boundary.md`. Short version:

1. Zig side: `export fn wmc_<name>(...)` in `zig/src/root.zig`, C types only, prefixed
   `wmc_`, doc comment says what it computes and its determinism guarantee.
2. Rust side: matching declaration inside `unsafe extern "C"` in
   `crates/zig-kernels/src/lib.rs`, then a **safe wrapper** that takes slices, asserts
   lengths, and has a `// SAFETY:` comment.
3. A test on both sides. Bump `abi_version` (Zig) and `ABI_VERSION` (Rust) together
   when any signature changes.
4. Buffers are caller-owned, non-overlapping unless documented, and the kernel never
   retains pointers.

## Definition of done

`make ci` green (fmt, clippy -D warnings, zig fmt, zig tests, cargo tests). For anything
touching a hot path: a criterion before/after number in the commit message and
`docs/PERF.md` updated. For any new system: a determinism test across thread counts.
