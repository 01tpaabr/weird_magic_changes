# Rust <-> Zig FFI boundary rules

The boundary is `zig/src/root.zig` (exports) and `crates/zig-kernels/src/lib.rs`
(imports + safe wrappers). Nothing else in the repo may declare `extern` or write `unsafe`
FFI calls.

## Types that may cross

| Rust                 | Zig              | Notes                                         |
|----------------------|------------------|-----------------------------------------------|
| `u8..u64,i8..i64`    | `u8..u64,i8..i64`|                                               |
| `usize`              | `usize`          | lengths, counts, indices                      |
| `f32`, `f64`         | `f32`, `f64`     |                                               |
| `bool`               | `bool`           | fine in practice; prefer `u8` flags in structs |
| `*const T`, `*mut T` | `[*]const T`, `[*]T` | always paired with a `usize` len          |
| `#[repr(C)] struct`  | `extern struct`  | POD only; derive `bytemuck::Pod, Zeroable`    |
| `#[repr(u32)] enum`  | `enum(u32)`      | give every variant an explicit value           |

Never: Rust `&[T]`, `Vec`, `String`, `Option`, `Result`, generics, trait objects;
Zig slices `[]T`, optionals `?T`, error unions `!T`, non-extern structs, `anytype`.

## Contract for every kernel

- Caller (Rust) owns all memory and guarantees validity for `len` elements.
- Input and output buffers do not alias unless the doc comment says the kernel is
  in-place-safe.
- The kernel does not allocate, does not keep pointers, does not touch globals, does not
  spawn threads, does not call libc, does not panic on valid input (it may `assert` in
  ReleaseSafe/Debug; assertions are compiled out in ReleaseFast, so the Rust wrapper must
  enforce preconditions itself with `assert!`).
- Deterministic: same input bytes => same output bytes. Reduction order fixed. No
  `@setFloatMode(.optimized)` unless the doc comment says results are approximate.

## Adding a kernel (checklist)

1. `zig/src/kernels/<name>.zig`: `pub fn` over slices + tests vs scalar reference.
2. `zig/src/root.zig`: `pub const <name> = @import("kernels/<name>.zig");` and
   `export fn wmc_<name>(...)` that converts many-pointers to slices and calls it.
3. `crates/zig-kernels/src/lib.rs`: add to `unsafe extern "C" { }`, write a safe `pub fn`
   that takes slices, `assert_eq!`s lengths, and calls with `// SAFETY:`.
4. Test in Rust that calls the wrapper (empty input + odd length at minimum).
5. If any existing signature changed: bump `abi_version` in Zig and `ABI_VERSION` in Rust.
6. `make ci`.

## Optimize modes

`build.rs` maps cargo opt-level -> Zig mode: 0 => Debug, 1 (our dev profile) =>
ReleaseSafe (bounds checks on, still fast), 2/3 => ReleaseFast. Override:
`WMC_ZIG_OPTIMIZE=ReleaseSafe cargo build --release` to hunt UB in a release build.
Both sides build for `cpu=native` / `target-cpu=native` so SIMD widths agree.

## Debugging a crash at the boundary

- Build with `WMC_ZIG_OPTIMIZE=Debug` to get Zig's safety checks + stack traces.
- `cargo test -p zig-kernels` isolates the wrapper layer.
- Check `wmc_abi_version()` first; a stale `.a` in `target/` is the most common cause.
  `make clean` if in doubt.
