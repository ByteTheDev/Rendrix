# Release executable size

The historical **13.8 MB** figure was a *packaged Windows app* reported in
[PERFORMANCE_AUDIT.md](PERFORMANCE_AUDIT.md) under the old profile (`lto = "thin"`,
`codegen-units = 4`, `opt-level = 3`). It is not the bare `rendrix-runtime.exe` and is not comparable with
current figures. A build with the current profile was measured on 2026-11-16 (see "Integration measurement"
below); the estimates in the table are still qualitative expectations from typical Rust projects, not
Rendrix measurements. Treat them as hypotheses unless a like-for-like A/B is recorded.

The knobs live in the workspace root, [`engine/Cargo.toml`](../engine/Cargo.toml)
`[profile.release]`. That is the profile used by `npm run runtime`, `npm run bench`,
and CI's `cargo build --release -p rendrix-runtime` step. Dev and test builds inherit
`[profile.dev]` (`opt-level = 1`) and are intentionally left fast and usable.

## Changes applied

| Setting | Before | After | Expected size impact (estimate) | Build-time cost | Correctness / debugability trade-off |
|---|---|---|---|---|---|
| `lto` | `"thin"` | `"fat"` | Moderate-to-large: full LTO sees the whole crate graph at once, so it can inline across crate boundaries and drop code thin LTO leaves behind. Expect a noticeable reduction, not a guaranteed percentage. | **High.** Fat LTO is significantly slower to link than thin LTO — the dominant cost of this change, especially on the three CI platforms. | None at runtime. Debug info/symbols are a separate concern; LTO can make optimized stacks harder to read, but behavior is unchanged. |
| `codegen-units` | `4` | `1` | Small-to-moderate on its own, and it compounds with fat LTO: one unit per crate means whole-crate visibility for inlining and dead-code elimination. | **Moderate.** Less parallelism while compiling each crate; longer cold builds. | None. |
| `strip` | unset (`"none"`) | `"symbols"` | Potentially the largest single win on Windows. Release builds already default to no DWARF debug info, so `"debuginfo"` would remove almost nothing; `"symbols"` removes the COFF symbol table/exported symbol metadata as well. | Negligible (one extra pass at link time). | **Real trade-off:** stripped executables produce much less useful crash stack traces and are harder to attach a profiler/debugger to. Panic *messages* still print (the runtime's panic hook is unaffected), but symbolized backtraces are gone. Mitigation: archive an unstripped release binary with every shipped build if post-mortem debugging matters. |
| `opt-level` | `3` | `3` (unchanged) | — | — | Kept deliberately. Dropping to `"s"`/`"z"` would shrink further but trades away renderer throughput. See *Options considered but not applied*. |

## Why not `panic = "abort"`

`panic = "abort"` removes unwinding tables and panic runtime machinery, so it is a real
size lever. It was **not** enabled here because it changes runtime semantics, and the
task calls for changes that do not:

- Unwinding is what runs `Drop` destructors on panic. Switching to abort means a panic
  terminates immediately, leaving cleanup (file handles, GPU/JS state, threads) undone.
- `std::panic::catch_unwind` / `resume_unwind` stop working. A search of the engine
  found no direct uses today, but the `rendrix-web` crate installs a
  `std::panic::set_hook` that logs `[rendrix] panic: ...`; the hook still fires on
  abort, but there is no longer any recovery path after it.
- Third-party crates (`rquickjs`, `wgpu`, `image`) are large and may rely on unwinding
  for internal error paths or at FFI boundaries. An abort there converts a recoverable
  condition into process termination — a behavioral change I cannot validate without
  running the app, which is out of scope for this edit.
- CI and `package.json` run benchmarks under `--release`
  (`cargo run --release -p rendrix-runtime ... -- bench`), so abort would change the
  benchmark configuration too.
- `cargo test` itself is **not** the blocker: Cargo forces the test profile to unwind,
  so release `panic = "abort"` would not break ordinary tests. The concern is runtime
  and benchmark behavior, not the test harness.

Recommendation: measure the size delta in isolation first. If it is compelling, gate it
behind a dedicated `release-lto`/`release-size` profile used only for shipping, and run
the full e2e/WPT suite plus a panic-path smoke test before adopting it broadly.

## Options considered but not applied

- **`opt-level = "s"` / `"z"`.** Optimizing for size instead of speed could reduce the
  binary further, but Rendrix is a rendering engine where frame time matters; silently
  trading throughput for megabytes would be a poor default. If measurements show the
  LTO/strip changes are insufficient, add a separate `[profile.release-size]` (with
  `inherits = "release"`, `opt-level = "z"`) and compare, rather than changing the
  production profile.
- **`strip = "debuginfo"`.** Release builds default to `debug = 0` / no DWARF, so this
  removes almost nothing on the platforms we ship. `"symbols"` is the meaningful
  setting for a shipped binary.
- **`debug = true` / keeping PDBs.** Explicitly enabling debug info would *increase*
  distribution size; instead, archive an unstripped artifact out-of-band if needed.
- **`lto = true` vs `"fat"`.** These are equivalent aliases in Cargo; `"fat"` is used
  for clarity.

## Effects on other build targets

- **Dev / test:** unchanged. `[profile.dev]` keeps `opt-level = 1` and Cargo tests build
  in the fast test profile, so ordinary `cargo test` / `npm test` do not inherit fat LTO
  or single-unit builds. Only `cargo build --release` pays the compile-time cost.
- **Benchmarks:** `npm run bench` uses `--release`, so it gets the new profile (including
  the slower link and stripped symbols). Timings should otherwise be unaffected because
  `opt-level` stays at 3.
- **WASM (`rendrix-web`):** `wasm-pack --release` also reads `[profile.release]`, so fat
  LTO and `codegen-units = 1` apply there too (`wasm-opt` remains disabled via
  `[package.metadata.wasm-pack.profile.release]`). Stripping WASM symbols is generally
  harmless; verify the WPT pass rate after integration if the WASM artifact is shipped.
- **Windows PDBs:** the MSVC toolchain only emits PDBs when debug info is enabled, which
  is not the release default here; `strip = "symbols"` removes symbol data from the
  executable itself.

## Integration measurement (2026-11-16)

The profile was built and measured on the reference machine (Windows 11, RTX 3060):

- The release build with fat LTO + one codegen unit took **4 m 13 s** (cold, all crates
  rebuilt). This is the main cost of the change; dev/test builds were unaffected and
  `cargo test --workspace` stayed fast.
- `engine/target/release/rendrix-runtime.exe` was **15.14 MB** (15,877,632 bytes) with the
  new profile. A later release binary in this working tree, built 2026-11-22, measured
  **16,054,272 bytes** (15.31 MiB). Later commits changed version metadata (including one engine prelude
  version string) and npm package scope, not rendering code. These are single observations, not a controlled
  series.

**This is not evidence that the profile reduced size.** The historical 13.8 MB figure in
PERFORMANCE_AUDIT.md refers to a *packaged app* produced by `rendrix package`, not the bare
`rendrix-runtime.exe`, and predates the canvas/compositing/3D/wasm work that has since grown
the codebase. A fair A/B requires building both profiles from the *same* source revision into
separate target directories and comparing like-for-like artifacts; that has not been done.
Until then, treat the size estimates above as unverified hypotheses.

The stripped binary was smoke-tested indirectly by running the full `bench 1000` scenario
(headless GPU render, e2e-style interaction) without issues; a dedicated panic-path check of
the panic hook output is still outstanding.

## Remaining verification

1. A/B the two profiles on one source revision (separate `CARGO_TARGET_DIR`s) and record
   real sizes for both the bare runtime and the packaged app.
2. Check the WASM artifact (`wasm-pack --release`) and the curated WPT pass rate, since it
   inherits `[profile.release]`.
3. Re-check panic-hook output on a deliberately panicking release build.
4. If the size win is negligible, prefer reverting to the faster thin-LTO profile, or gate
   fat LTO behind a separate `release-size` profile used only for shipping.
