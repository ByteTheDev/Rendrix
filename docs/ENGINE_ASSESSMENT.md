# Rendrix engine transformation — Phase 0 assessment and roadmap

Date: 2026-11-13. Checkpoint tag: `checkpoint/pre-engine-transformation` (HEAD `8d4bd59`).
Direction change: Rendrix is a standalone rendering engine and application runtime for TypeScript
apps. It is **not** an Electron alternative, wrapper, or compatibility layer.

> **Historical document (2026-11-13, with a status update on 2026-11-16).** Sections 1–2 and 6 describe the
> pre-rewrite tree at checkpoint `8d4bd59` and the first milestone; they are preserved as decision history,
> not as the current state. Section 3's roadmap has been overtaken by releases 0.5.0 and 0.5.1. For what is
> true today, use [ARCHITECTURE.md](ARCHITECTURE.md), [COMPATIBILITY.md](COMPATIBILITY.md),
> [ROADMAP.md](ROADMAP.md) and [PERFORMANCE_AUDIT.md](PERFORMANCE_AUDIT.md).

## 1. What existed at the checkpoint (audited, not assumed)

| Area | Location | What it really is | Verdict |
|---|---|---|---|
| Authoring model | `packages/cli/src/html.mjs`, `css.mjs`, `compiler.mjs` | Build-time (Node/Bun) parser of a *supported subset* of HTML + CSS (PostCSS) that **resolves styles statically** into a `package::Node` tree stored in a JSON `.rdx` bundle. | Conflicts with the new goal: no runtime DOM, no runtime cascade, no pseudo/selector engine at runtime. Kept as legacy only. |
| App logic | `packages/cli/src/backend.mjs`, `native/src/runtime.rs` | A TypeScript file exporting `{ state, actions }`, transpiled to CommonJS with `ts.transpileModule`, run in QuickJS. Actions mutate a state map; the UI is bound via `data-bind` attributes. **No DOM access exists from JS.** | Declarative-state demo, not a web-style runtime. QuickJS integration (`rquickjs 0.14`) and its limits/permission model are reusable ideas. |
| Renderer (M0) | `native/src/gpu/*` (`winit 0.30`, `wgpu 25`, `taffy 0.9`, `cosmic-text 0.14`) | Walks the static `Node` tree → taffy → rounded-rect SDF quads + CPU glyph atlas. Re-rasterizes the *entire atlas every frame*; full re-layout + repaint every vsync; no images, gradients, shadows, transforms, scrolling, text editing, or animation; `display:grid` is laid out as block; `onclick` only logs to stdout (QuickJS not wired). 9 backend tests pass. | Proves winit+wgpu+taffy+cosmic-text work on Windows; the shader approach (SDF rounded rect) is sound. Code is tied to `package::Node` and a static tree, so it is superseded rather than extended. |
| Legacy host | `native/src/main.rs`, `view/*` | GPUI 0.2.2 renderer (deprecated in the previous charter). | Conflicts with "Rendrix owns the pipeline". Slated for removal. |
| Electron compat | `packages/cli/src/electron-compat/**` (≈15k lines incl. a 10k-line API surface snapshot), `import-electron.mjs`, `native/src/{tray,hotkeys,capabilities,platform,image_store}.rs` partially, examples/electron-sample*, many docs | Shims emulating Electron 33's JS API on top of the host call protocol. | Directly contradicts the new direction (no Electron API compatibility). Removed after the new stack passes the milestone (§4). |
| Native platform | `native/src/platform.rs`, `tray.rs`, `hotkeys.rs`, `updater.rs` | Clipboard, dialogs (`rfd`), power blockers, tray, global shortcuts, updater — all real Windows/macOS/Linux code. | Reusable. Migrated into the new platform layer on demand (clipboard/dialogs first). |
| CLI | `packages/cli` | `init/dev/build/compile/host/doctor/preview/sign` and cross-target packaging that appends the bundle to a prebuilt host executable (PE/ELF/Mach-O). | Packaging, signing and host-fetching logic is valuable and kept; commands are retargeted to the new runtime. |
| Tests | `packages/cli/test/*.mjs` (node:test), Rust unit tests in `native` | Mostly CLI/compiler/Electron tests. | New engine gets its own Rust test suites per crate. |

### Dependencies on Electron / Chromium / WebView
* **WebView:** already removed (branch `backup/webview-era`); no `wry`/`tao`/`gtk` in the tree.
* **Chromium:** none linked. `electron-33.json` is a *snapshot of API names*, not code.
* **Electron:** compat shims + importer + docs + two example projects only (§ table). No engine
  code depends on them at compile time except `capabilities.rs`/`runtime.rs` host-call plumbing
  that the shims were built on.

### How TypeScript becomes executable code today
`backend.ts` → TypeScript compiler API (`transpileModule`, target ES2020, CommonJS) in the Node/Bun
CLI → string embedded in the `.rdx` JSON → `rquickjs` `Context::eval` at runtime. Module resolution is
a build-time CommonJS concatenation limited to relative `.ts` files. No ES modules at runtime, no
bare specifiers, no TSX, no async job pump beyond actions.

### Limitations that block the vision
1. No DOM and no JS ↔ UI object model; UI cannot be created or mutated at runtime.
2. CSS resolved at build time; no selector matching, cascade, inheritance, variables, media queries,
   transitions or animations at runtime.
3. Renderer is a repainted-every-frame M0 (no dirty tracking, per-frame atlas rebuild).
4. No event loop (timers, microtasks, rAF), no Web APIs.
5. Layout only maps a flat CSS subset to taffy; no inline formatting context (text runs inside
   inline elements), no scrolling/overflow, no grid.
6. Two hosts + Electron layer = three overlapping runtimes.

## 2. Target architecture

A Cargo workspace at `engine/`; every crate has one job and a narrow interface.

```
rendrix-css      CSS tokenizer/parser, selectors, specificity, values, cascade, computed style, media queries, keyframes
rendrix-dom      Arena DOM: nodes, attributes, tree ops, mutation records, selector queries, HTML parser, form state
rendrix-layout   LayoutEngine trait + Taffy backend (flex/grid/block + inline formatting contexts via a TextMeasurer trait)
rendrix-gfx      DisplayList, text shaping/atlas (cosmic-text), wgpu renderer (SDF shapes, glyphs, clips), offscreen capture
rendrix-core     Engine: DOM → style → layout box tree → display list; hit testing, focus, scrolling, transitions/animations
rendrix-js       JsEngine trait + QuickJS backend, ES module loader, event loop (tasks/microtasks/timers/rAF), DOM bindings, Web APIs
rendrix-runtime  Platform shell: winit window, input → DOM events, frame scheduler, app bundle loader; `rendrix-runtime` binary
```

Boundaries:
* **JS engine is replaceable:** `rendrix-js` exposes a `JsEngine` trait; only `quickjs.rs` names rquickjs.
  Web API surface is a JS prelude over the single native `__native(op, ...)` binding table, so another engine
  needs only the trait implementation plus that entry point.
* **Layout engine is replaceable:** `LayoutEngine` consumes a neutral box tree (`LayoutNode` + `ComputedStyle`),
  not DOM types. Taffy is one implementation.
* **TypeScript:** compiled to ES-module JavaScript before execution (build step in the CLI using the
  TypeScript compiler API; the runtime never sees TS). The runtime loads ES modules from a bundle
  module map via QuickJS's module loader; bare specifiers are resolved by the CLI.
* **App format:** `rendrix-app/1` bundle (JSON: `html`, `modules`, `assets`, window config). The packager
  appends it to a prebuilt `rendrix-runtime` executable, reusing the existing PE/ELF/Mach-O trailer logic.

## 3. Roadmap and status at Milestone 1 (2026-11-14)

This table records status at the milestone, not today; runaway-script limits, workers, WebAssembly, Canvas 2D,
SVG, WebSocket and the a11y bridge have all landed since. See [ROADMAP.md](ROADMAP.md) for current status.

| Phase | Scope | Status |
|---|---|---|
| 0 | Audit, target architecture, checkpoint | **Done** (this document; tag `checkpoint/pre-engine-transformation`) |
| 1 | Core runtime: JS execution, ES modules, TS compile, event loop | **Done for the milestone**: QuickJS behind a `JsEngine` trait, module loader, top-level await, timers/rAF/microtasks, unhandled-rejection reporting, build-time TS compile + type-check. Not done: runaway-script limits, workers, WebAssembly |
| 2 | Rendering foundation | **Done, with gaps**: wgpu SDF renderer (rounded rects, per-side/dashed borders, gradients, shadows, clips, transforms, images, text), persistent glyph atlas with LRU, display list, culling, one-draw-call frames, and offscreen compositing layers for true Gaussian `filter: blur()`, real `backdrop-filter` and per-pixel `clip-path: polygon()` (ordinary frames keep the one-draw-call path). Canvas 2D, SVG painting and an experimental 3D scene path reuse the same pipeline. Not done: dirty-rect presentation, full `filter` completeness, spatially-varying masks |
| 3 | Web platform | **Broad, not complete**: real DOM, CSS cascade/animations/at-rules (`@supports`, `@layer`, `@container`, nesting, `@import`), events, forms with validation and pickers, observers (Mutation/Resize/Intersection), Shadow DOM, custom elements, `Range`/`Selection`, `contenteditable`, `fetch`/cookies, WebSocket, Workers, storage, Canvas 2D subset. See [COMPATIBILITY.md](COMPATIBILITY.md) for exact gaps (full table/float fidelity, `writing-mode`, WOFF2, Service Workers, XHR/IndexedDB, media, `cqw`/`cqh`, complex masks) |
| 4 | Application platform | **Started, Windows-verified**: winit window, DPI, input, IME commit, clipboard, multi-window, OS file dialogs, native file drag-and-drop, touch and gamepad entry points, AccessKit bridge, update client, standalone executables. Not done: macOS/Linux validation, tray/global shortcuts, Android/iOS shells |
| 5 | Developer ecosystem | **Started**: `init/dev/build/run/check/package/render/doctor`, TSX with a direct-DOM JSX runtime, source maps, state-preserving hot reload, `--inspect` snapshot, a Solid adapter, an experimental wasm web target and mobile-input scaffolding. Not done at the milestone: React adapter, overlay inspector UI/devtools protocol. An experimental React adapter now exists but its real-runtime test is not green |
| 6 | Production readiness | **Not complete**: CI config builds/tests three OSes but no cross-platform results are recorded; no memory soak has been run; signing/installer/manifest helpers are wired but unsigned and unrun on macOS/Linux; the AccessKit bridge exists but no manual screen-reader pass is recorded |

### Priorities (historical 2026-11-16 list, with 0.5.1 status)

This list is kept as decision history. Item 1 has since been worked on; the rest are still current, and
[ROADMAP.md](ROADMAP.md) is authoritative.

1. **Cold start — worked on in 0.5.1.** Template rewraps, parallel first-frame prewarm, a background font-DB
   load and headless-init overlap cut the first frame from 416.0 ms to 81.0 ms in medians of three runs
   **taken under concurrent load**; the `< 0.2 s` goal is met in those runs. A quiet-machine absolute is
   still pending. See [PERFORMANCE_AUDIT.md](PERFORMANCE_AUDIT.md) §"Cold start re-measured and cut".
2. **Mutation latency under 3 ms (120 FPS) — open.** Incremental box generation, stylesheet caching and
   viewport-dependency pruning landed; the remaining ~4.4–7 ms is Taffy re-solve plus
   `InlineContent::key` rehashing.
3. **macOS and Linux validation** on real hardware, plus the manual screen-reader pass in [A11Y_CHECKLIST.md](A11Y_CHECKLIST.md). CI builds and tests all three desktop targets, but no cross-platform hardware results are recorded.
4. **Web-target gaps**: a browser run of the WebGL2 fallback (the renderer no longer requires shader storage
   buffers and has a native downlevel-limits pixel test, but no browser run has been recorded), browser-backed
   WebSocket/Workers/storage persistence, static import cycles and a runaway-script interrupt for the worker.
5. **Scale measurement**: 50k+ node documents and a second machine/GPU, to find the real O(n) wall beyond the 9,009-node reference benchmark.
6. **Compatibility gaps with high web value**: `cqw`/`cqh`, WOFF2, `writing-mode`, spatially-varying masks, table `colspan`/`rowspan`, float text wrap, the range input and a visual validation bubble.
7. **Ecosystem**: a React reconciler (the Solid adapter and state-preserving hot reload have landed) and an overlay inspector UI on top of the existing `--inspect` JSON snapshot.

## 4. Migration: what was removed and where its job went

Removed in the Milestone 1 change set (everything is recoverable from the checkpoint tag
`checkpoint/pre-engine-transformation`):

| Removed | What it did | Replacement / disposition |
|---|---|---|
| `packages/cli/src/electron-compat/**`, `import-electron.mjs`, Electron examples, Electron docs | Emulated Electron 33's API and imported Electron projects | **Dropped on purpose** (no compatibility goal) |
| `native/` GPUI host (`main.rs`, `view/*`), M0 `gpu/*`, `package.rs` | The previous renderer(s) and static `Node` IR | `engine/` (DOM → CSS → layout → wgpu) |
| `packages/cli/src/{html,css,compiler,backend,library,pack,native,hosts,preview}.mjs`, old templates, `types/app.d.ts` | Build-time static HTML/CSS compiler, `{state, actions}` backend model, host fetching | `packages/cli/src/app/*` (TS build pipeline, bundle, packaging) |
| `native/src/platform.rs` (clipboard, dialogs, power blockers, external links) | Native services | Clipboard re-implemented via `arboard`; native dialogs and external links implemented via `rfd`/host ops. Power blockers were not ported. |
| `native/src/{tray,hotkeys}.rs` | Tray icon, global shortcuts | **Not ported** (still open in [ROADMAP.md](ROADMAP.md) R2.3) |
| `native/src/updater.rs`, `packages/cli/src/signing.mjs` (kept), `installer/` (kept) | Update download/verify/install, signing helpers | Update client, signing and NSIS helpers are wired into `rendrix package`; only Windows has been exercised and no signed release has been recorded |
| `native/src/capabilities.rs` permission model | Filesystem/network/process grants | Re-implemented smaller in `rendrix-js/host.rs` (network, fs read/write); process grants intentionally not carried over |
| `scripts/*` (smoke/benchmark/drive scripts), `.github/workflows/*` | Old host smoke tests and release automation | `scripts/e2e.mjs`, `engine` benchmark, `.github/workflows/ci.yml` |

The website under `website/` has been rebranded to the engine product; it has no Electron-era copy. Its
documentation tabs are synced from `docs/` by `website/sync-docs.mjs`.

## 5. Verification policy
Every milestone: `cargo test --workspace`, a release build, an actual rendered screenshot inspected by
eye and compared against layout-derived expectations, and the interactive window run. A feature is
"done" only when exercised by a test or the demo app; everything else is listed in `COMPATIBILITY.md`
as partial/unsupported.

## 6. Milestone 1 results (2026-11-14)

**The milestone application** (`examples/app-counter`, TSX + CSS + flexbox) runs through Rendrix's own stack:
native winit window → TypeScript-compiled ES modules in QuickJS → DOM created and mutated by script → CSS
cascade, variables, `:hover`/`:active`, transitions and a media query → flexbox layout → wgpu renderer.
Click handlers update state and the DOM and the updated interface is re-rendered. No Electron, Chromium or
WebView is linked or launched anywhere.

How it was verified (all on this machine: Windows 11, RTX 3060, Vulkan):

* `cargo test --workspace`: 38 CSS, 22 DOM, 8 layout, 12 graphics (real GPU pixel checks), 34 core,
  27 JS runtime, 4 bundle + 10 session integration tests; `node --test packages/cli/test`: 9 tests
  (including a bundle run in the real runtime).
* `npm run e2e`: 26 scripted steps (hover, three clicks, state and class assertions, a mid-transition colour
  sample, resize into the `max-width: 480px` media query) all pass; screenshots were inspected by eye.
* The real window was started at 125 % DPI and driven with window messages posted to its own handle; the
  presented frame (`RENDRIX_DUMP_FRAME`) showed the count at 3 with the hover state after three clicks.
* `rendrix package` produced a 13.8 MB standalone executable that renders the app with no other runtime.

Measured with `npm run bench` (release build, 1,000 list items = 9,009 DOM nodes, 1000×700 at 1×).
Median per frame; "total" is CPU pipeline plus GPU submission and completion, excluding vsync:

| Scenario | total ms | style | layout | paint | GPU |
|---|---|---|---|---|---|
| idle (nothing changed) | 1.1 | 0 | 0 | 0 | 0.4 |
| hover moves between items | 1.2 | 0.03 | 0 | 0 | 0.4 |
| one wheel notch | 1.2 | 0.02 | 0 | 0.14 | 0.3 |
| toggle a class on one item | 7.5–8.4 | 0.06 | 5.7–6.3 | 0.2 | 0.5 |
| change text of one item | 7.4–8.9 | 0.01 | 5.7–6.8 | 0.2 | 0.5 |
| add + remove 10 items | 7.3–8.0 | 0.03 | 5.4–6.0 | 0.2 | 0.5 |
| JS `classList.toggle` from script | 8.0–8.4 | 0.06 | 6.1–6.3 | 0.2 | 0.7 |
| viewport resize (full restyle + relayout) | 39 | 24.5 | 12.4 | 0.2 | 0.7 |

Interactive scenarios fit a 60 FPS budget (16.7 ms); 120 FPS (8.3 ms) is met for hover and scrolling and
sits at the limit for mutations, whose cost is dominated by whole-tree box generation (next priority).
Cold start for this list is ~0.6 s plus a ~0.75 s first frame. Numbers vary by several tenths of a
millisecond between runs; they were measured only on one machine.

Bugs the verification work found and fixed (kept here because they shaped tests): text drawn double-spaced
(glyph offset counted twice), listener loss when wrappers were garbage-collected, `MutationObserver` subtree
matching done after removal, stale styles after re-parenting, a `.ts` compile error for non-JSX files, a
predictable PRNG behind `crypto.getRandomValues`, and a debug path that rendered in the wrong colour format.
