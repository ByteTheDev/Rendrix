# Changelog

## 0.5.1 — Startup, animation layout and architecture targets

Patch release on top of 0.5.0. Full details are in the 0.5.0 sections below; the changes since the
0.5.0 tag are:

- **Startup:** first frame cut roughly 5× (template rewraps and parallel text prewarm); font database loads
  on a background thread. See *Cold start (2026-11-22)*.
- **Layout:** animations stay on the incremental layout path instead of forcing a full re-solve.
- **Packaging:** 32-bit Windows (`win32-x86`), 32-bit Linux (`linux-x86`), ARMv7 (`linux-armv7`) and
  RISC-V 64 (`linux-riscv64`) targets join the existing x64/ARM64 packages.
- **CI:** rendrix-gfx tests run serialized on Linux, fixing an intermittent SIGSEGV.
- Engine/CLI/types packages now report version `0.5.1`.

## 0.5.0 — Rendrix becomes a rendering engine and application runtime

Direction change: Rendrix no longer targets Electron compatibility or a static HTML-subset compiler. It is
an independent engine that runs TypeScript applications with its own DOM, CSS engine, layout, GPU renderer
and runtime. See `docs/ENGINE_ASSESSMENT.md`.

### Added
- `engine/` Rust workspace: `rendrix-css`, `rendrix-dom`, `rendrix-layout`, `rendrix-gfx`, `rendrix-core`,
  `rendrix-js` (QuickJS behind a `JsEngine` trait), `rendrix-runtime` (winit window, headless driver,
  benchmark, standalone packaging).
- Web platform layer: DOM, events, forms/text editing, selectors, `fetch`, `URL`, storage, observers,
  transitions/animations, timers/`requestAnimationFrame`, images (see `docs/COMPATIBILITY.md`).
- New CLI: `rendrix init | dev | build | run | check | package | doctor` with TypeScript type-checking, TSX
  (`rendrix/jsx-runtime`), `node_modules` ESM resolution and `.rdxapp` / standalone executable output.
- Multi-architecture Windows packaging: `--target win32-x86` (32-bit, `i686-pc-windows-msvc` /
  `i686-pc-windows-gnu`, aliases `x86` / `ia32`) joins `win32-x64` and `win32-arm64`; x64 is hardware-verified,
  the other two are compile-checked in CI but not run here.
- Multi-architecture Linux packaging: `--target linux-x86` (`i686-unknown-linux-gnu`), `linux-armv7`
  (`armv7-unknown-linux-gnueabihf`, aliases `armv7` / `armhf`, the Raspberry Pi 32-bit target; 32-bit ARM
  hosts, which Node reports as arch `arm`, auto-detect to it) and `linux-riscv64`
  (`riscv64gc-unknown-linux-gnu`, alias `riscv64`) join `linux-x64` / `linux-arm64`; all three are
  compile-checked in CI only, not run on hardware here.
- `examples/app-counter` and a scripted end-to-end test (`npm run e2e`); `npm run bench`.
- Offscreen compositing layers: real `backdrop-filter`, true Gaussian `filter: blur()` and per-pixel
  `clip-path: polygon()` (ordinary frames keep the single-draw-call path).
- CSS text: `word-break` and `-webkit-line-clamp` (line-limited content with a trailing ellipsis).
- CSS at-rules: real `@supports` condition evaluation, `@layer` ordering (reversed for `!important`),
  and `@container` size queries against `container-type` ancestors.
- `:focus-visible` follows input modality (keyboard/a11y vs pointer).
- Borders: per-side colours and real dashed/dotted rendering in the shader.
- Web APIs: `document.cookie` sync with the session jar plus `Max-Age`/`Expires` expiry;
  `crypto.subtle.digest` (SHA-1/256/384/512); Canvas `toBlob`, `OffscreenCanvas` and
  non-`source-over` `globalCompositeOperation`.
- Form pickers: `input[type=file].showPicker()` uses the OS file dialog, with in-engine popups for
  `<select>` and `input[type=date]`.

### Renderer correctness and scale (2026-11-16)
- Fixed multi-mesh and multi-scene 3D rendering: frame/mesh uniforms now use 256-byte dynamic offsets, so each draw keeps its own transform, material and camera. Covered by a two-mesh pixel test.
- `opacity` on a subtree now creates an isolated offscreen group, so overlapping translucent children blend correctly instead of pairwise against the backdrop.
- `backdrop-filter` no longer double-composites the backdrop when the page background is translucent.
- Blur is a separable two-pass Gaussian (14 taps, ~±3.5σ) instead of a truncated 5×5 kernel. Layer frames share one GPU snapshot of the painted prefix, one encoder and one submit; the offscreen target pool is capped at 32 with LRU eviction.
- 3D scene retrieval is a cheap `Arc` clone instead of deep-copying every mesh (up to ~1M vertices) twice per frame.
- Small images (≤512 px per edge) pack into a shared atlas and merge into one draw batch; large images and dynamic canvases keep dedicated textures.
- All passes in a layered frame (flat runs, backdrop snapshots, blur, composite, blit) are recorded in display-list order into one encoder and submitted once; flat content after a layer paints above it again, and a layer's backdrop excludes later content. Pooled layer targets are cleared to transparent each frame, and `backdrop-filter` grayscale/brightness now affect the backdrop.
- Fixed zero-size `render_to_rgba`, frame-wide profiling counters for layered frames, and mid-frame glyph-atlas recycling inside nested layers.

### Performance and web target (2026-11-18)
- First-frame shaping: `nowrap`/`pre` text now shares one shaping across min-content, max-content and definite-width queries instead of up to three, with a geometry-identity test. The paragraph LRU grows to 8192 entries so a ~10k-key first frame no longer thrashes; small apps still allocate only what they use. The benchmark's startup breakdown now prints frame-1 cache hits and cache size.
- WebGL2 fallback: the renderer no longer needs shader storage buffers. Instance data is read from a 48-entry uniform array with chunked draws, and polygon clipping uses a uniform buffer, both selected from device limits so the native WebGPU path is byte-for-byte unchanged. A native regression test renders identical pixels under `downlevel_webgl2_defaults()`. A real no-WebGPU browser run is still unrecorded.
- The two superseded ignored Session-level tests now print why they were replaced instead of panicking with `todo!()`, so `cargo test -- --ignored` stays green.

### Cold start (2026-11-22)
- First-frame text no longer re-runs rustybuzz per width: the first fully shaped paragraph per content becomes a *template*, and every other width (min-content, max-content, definite) re-wraps a clone of its cosmic-text buffer. A geometry-identity test (`rewrap_is_measurement_identical`, over wrapping/CJK/hard-break/multi-size content) pins the clone+rewrap bit-for-bit to a fresh shape; the probe measured it ~5–15× cheaper. `TextSystem` gains `rewrap_count`/`rewrap_nanos`, and `shape_count` now counts full shapes only.
- The first frame prewarms in parallel: after the first box build the engine hands every inline content to `TextSystem::prewarm_parallel`, which shapes them across worker threads (each with its own `FontSystem` built from a cloned font database) plus the width-0 variants by rewrap, and inserts both under the exact cache keys Taffy will query. On-demand identity is test-pinned; below 16 fresh contents the prewarm no-ops, and a failed worker degrades to on-demand shaping. New `EngineStats::first/last_prewarm_micros` keep the phase attributable.
- The font database now loads on a background thread (disk cache or full system scan), overlapping DOM parse + JS evaluation; the first font use joins it, and a dead loader thread falls back to a synchronous load. `TextSystem::fonts` is private behind an `ensure_loaded` accessor as part of this.
- The headless driver builds its `Session` while the GPU context and renderer initialize on a worker thread (both phases were serial). The windowed driver keeps serial init for now (surface/event-loop ordering is untestable from a headless track) — noted as a follow-up.
- `bench 1000` medians under concurrent load (two other agents building on the same machine; relative signals — re-measure quietly): startup 684.6 → 419.5 ms, first frame 416.0 → **81.0 ms** (the `< 0.2 s` goal), 4,017 full shapes → 2,008 full + 2,009 rewraps executed in ~37–42 ms of prewarm wall time, first Taffy layout 383.4 → 14.5 ms (measurement cache hits), display list unchanged (119 items / 1 batch / 773 instances). The startup breakdown prints the new rewrap, prewarm and background-load rows.

### Removed
- Electron compatibility layer, Electron project importer and their docs/examples.
- GPUI legacy host and the M0 static-tree renderer (`native/`), the build-time HTML/CSS compiler and the
  `{ state, actions }` backend model, old host fetching/packaging scripts, old CI workflows.
  Migration notes: `docs/ENGINE_ASSESSMENT.md` §4.

### Known limitations (as of the 2026-11-11 rewrite checkpoint)
Windows was the only verified platform and there was no accessibility tree at that checkpoint. Both have
since changed: an AccessKit tree and OS bridge are implemented, but no manual screen-reader pass has been
recorded. See [docs/COMPATIBILITY.md](docs/COMPATIBILITY.md) and
[docs/A11Y_CHECKLIST.md](docs/A11Y_CHECKLIST.md) for the current state.

## Historical — the 2026-11-11 rewrite decision

This section records a decision, **not** the current state of the product. On 2026-11-11 the owner
authorized replacing the WebView/Electron-compatibility direction with an independent rendering engine;
the pre-rewrite tree is preserved on branch `backup/webview-era` (tag `webview-era-checkpoint`, commit
`0fc860b`). The plan was executed by the 0.5.0 release: the Electron compatibility layer, Electron importer,
WebView host and legacy GPUI host were removed, and `engine/` became the only engine. The bundle trailer
magic is `RENDRAPP` (`engine/crates/rendrix-runtime/src/bundle.rs`), the app format is `rendrix-app/1`, the
CLI command is `rendrix`, and the home-directory override is `RENDRIX_HOME`.

The detailed "Unreleased" notes that once lived here described intermediate states (a still-working legacy
host, an Electron importer, M1–M3 feature ladders) that no longer exist; they were removed rather than left to
be read as current. Entries below this section are release history as originally written under the old name.

## 0.4.0

First stable release of the 0.4.0 series, graduating the beta.4 work (full Electron compatibility coverage, native UI/menus, host builds). See the beta.2 notes below for the feature set.

## 0.4.0-beta.2

This is a beta. Interfaces may change before 0.4.0, and the platform hosts for macOS and Linux have been built by CI but not yet run by hand.

0.4.0-beta.2 supersedes 0.4.0-beta.1, which is deprecated. The beta.1 package was published with a Linux host build failure: the `xattr` dependency did not compile on Linux (fixed by vendoring `xattr` 0.2.3 under `native/patches`).

### Added

- **Electron import.** `rendrix import electron <dir>` converts an Electron project into a Rendrix project and writes a review report. `rendrix run`, `build` and the target shortcuts also accept Electron project directories. The importer does not modify the source project.
- **Electron compatibility runtime.** `require('electron')`, `fs`, `path`, `os`, `child_process`, `https` and `net` are provided by shims. Every Electron 33 export is classified as implemented, partial or unsupported in the coverage ledger, and a test fails when the ledger is out of date.
- **Electron shims on the native renderer.** The native renderer runs the same shims through new `api.native` operations: `app`, `clipboard`, `dialog`, `shell`, `net`, `power` and `screen`.
- **Named filesystem roots.** `api.native.filesystem` operations accept `{ root, path }`. The root can be a permission root, `userData`, or a file the user picked in a dialog. Picked files are granted for that one file only.
- **WebView renderer.** Set `renderer: "webview"` to render with the platform browser engine (WebView2, WKWebView or WebKitGTK).
- **Prebuilt hosts and packaging.** Hosts for six targets are downloaded and verified against `hosts.json`. Windows packaging can produce a signed executable and an NSIS installer.

### Changed

- Clipboard, dialog, power-blocker, external-link and HTTPS code is shared by both hosts in `native/src/platform.rs`.
- The browser preview's `api.native` has the new operations, which reject with a message that they need the native or WebView host.
- `fs.readdir` returns names, as Electron does. `withFileTypes` returns entries with `isDirectory()` and `isFile()`. `fs.stat` reports `mtime` and `mtimeMs`.
- `userData` is created when the app starts, so the first write to it succeeds.

### Fixed

- A backend action can await a host call that was started while the backend loaded, such as `app.whenReady()`. Previously the action failed with "did not finish".

### Known limits

- The native renderer has one window and no page. `window.create`, `window.load` and `ipc.invoke` from the backend are unavailable. `webContents.send` messages are dropped with a warning, and close listeners cannot cancel a close.
- Tray, global shortcuts, cursor position and display events were partial or unsupported in the old Electron-compatibility layer. The coverage ledger was removed with that layer in 0.5.0; tray and global shortcuts are still unimplemented in Rendrix (see [ROADMAP.md](docs/ROADMAP.md)).
- GPUI does not report a scale factor per display, so the native renderer reports the app window's scale factor for every display.
- The browser preview does not run the Electron shims.
- Clipboard, dialogs and power blockers work on Windows only.
- Accessibility: native `aria-*` and `role` metadata is preserved but not exposed to the OS. GPUI 0.2.2 has no accessibility API, so this needs an upgrade or bridge layer; scope before promising screen-reader support.
- Motion on the native path: CSS transitions, animations and transforms are reported as unsupported. A GPUI animation primitive would close this; otherwise use the WebView renderer for motion.
- Signing and update support: packages are produced unsigned by default; signing, notarization and in-app updates are handled outside the tool or remain roadmap work.

The old `docs/SUPPORTED_SYNTAX.md` `api.native` reference was removed with the Electron-compatibility layer in 0.5.0; current capabilities are listed in [docs/COMPATIBILITY.md](docs/COMPATIBILITY.md).
