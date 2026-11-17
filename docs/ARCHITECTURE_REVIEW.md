# Rendrix Architecture Review

Date: 2026-11-14. Method: three parallel read-only subagent investigations (runtime/language, rendering/graphics, platform/application) cross-checked against the four existing audits (`DEPENDENCY_AUDIT.md`, `ENGINE_ASSESSMENT.md`, `IMPLEMENTATION_AUDIT.md`, `PERFORMANCE_AUDIT.md`) plus `ARCHITECTURE.md`, `COMPATIBILITY.md`, `ROADMAP.md`, `ACCESSIBILITY_PLAN.md`, and spot-verified against source (`engine/crates/*/src`, `packages/cli/src/app/build.mjs`, `engine/Cargo.toml`).

Constraints honored: no application code modified, no systems deleted or replaced, no unnecessary dependencies, all existing functionality preserved. All numbers below are quoted from `PERFORMANCE_AUDIT.md` / `ENGINE_ASSESSMENT.md` §6 (Windows 11, RTX 3060, Vulkan, 1000 items = 9,009 DOM nodes). No new benchmarks were invented.

> **Superseded (2026-11-16).** This is a historical snapshot from 2026-11-14. Its "verified absent" list
> (§2.3) and several partial rows in §2.2 are no longer accurate: Canvas 2D, WebSocket, Workers,
> IntersectionObserver, Shadow DOM, custom elements, `:has()`, AccessKit, multi-window, native dialogs,
> drag-and-drop, source maps, hot reload, `--inspect`, the Solid adapter and the wasm web target have all
> landed since. The `getContext('2d') === null` / `WebSocket === undefined` spot-checks in §1 were true at
> the time but are now stale. Use [ARCHITECTURE.md](ARCHITECTURE.md) for the current design,
> [COMPATIBILITY.md](COMPATIBILITY.md) for the feature matrix, [ROADMAP.md](ROADMAP.md) for status and
> [PERFORMANCE_AUDIT.md](PERFORMANCE_AUDIT.md) for measurements. This document is retained for the
> decision history and the "build vs retain" rationale in §6, which still holds.

## 1. Current architecture (verified)

```
TS/TSX/JS ──build──▶ ES2022 ESM bundle (rendrix-app/1 JSON)
  packages/cli/src/app/build.mjs (typescript compiler API, transpile + type-check)

bundle ──run──▶ Session ──▶ JsRuntime (QuickJS) ──▶ DOM ──▶ restyle ──▶ box tree ──▶ Taffy ──▶ display list ──▶ wgpu
  engine/crates/rendrix-runtime/src/{bundle,session,window,headless}.rs
  engine/crates/rendrix-js/src/{quickjs,runtime,host,engine_trait}.rs + prelude/*.js
  engine/crates/rendrix-{dom,css,layout,gfx,core}/src/
```

Replaceable boundaries (already exist, verified): `JsEngine` trait (`rendrix-js/src/engine_trait.rs:91-109`, only `quickjs.rs` names `rquickjs`); `LayoutEngine` trait over neutral `LayoutNode` + `ComputedStyle` (Taffy is one backend); text measured through `TextMeasurer`, shaped only in `rendrix-gfx`. Single native entry `__native(op,...)` (`quickjs.rs:107-120`, `runtime.rs:39`) with Web APIs in JS prelude. Confirmed: `WebSocket/XHR/IntersectionObserver/WebGL === undefined`, `getContext('2d') === null` (`tests.rs:423-424`, `prelude/dom.js:574`); no `accesskit/rfd/tray/hotkey` in `engine/`; single `struct App` holding one `Window`/`Session` (`window.rs:23`).

## 2. Consolidated status

### 2.1 Implemented (keep, do not rewrite)

- **JS/TS pipeline:** ES2023 via QuickJS-ng, ESM + TLA + dynamic `import()` loading, `transpileModule` ES2022 for `.ts/.tsx/.mts/.jsx/.js/.mjs`, type-check gated build, bare-specifier resolution at build time, `rendrix/jsx-runtime` direct-DOM JSX. Evidence: `build.mjs:11,106-214`, 27 JS tests, 9 CLI tests.
- **Event loop:** microtasks + `queueMicrotask`, platform timers, frame-driven rAF, unhandled-rejection tracking, `structuredClone/atob/performance/matchMedia/console`. Evidence: `quickjs.rs:130-146,202-212`, `runtime.rs:74-104,162-198`, `prelude/web.js`.
- **DOM/CSS/layout:** arena DOM with dirty tracking + GC, full cascade/inheritance/custom-props/media-queries/keyframes, flex/grid/block via Taffy with persistent incremental results, SDF wgpu renderer (usually one draw call), cosmic-text shaping + paragraph cache + 2048×2048 glyph atlas, display-list culling + hit testing, overlay scrolling, idle-skip scheduler (`AutoVsync`, `Wait/WaitUntil`). Evidence: `rendrix-dom/src/lib.rs`, `rendrix-css/src/{parser,cascade,selector,style}.rs`, `rendrix-layout/src/taffy_engine.rs`, `rendrix-gfx/src/{renderer,text,display}.rs`, `rendrix-core/src/lib.rs:Engine::update`.
- **Platform baseline:** one native window (DPI/resize/title/close), mouse/keyboard/wheel/IME-commit, text-only clipboard (`arboard`), permission-gated `fs_read/write/exists/readDir`, `ureq`-threaded `fetch` (relative/data:/http), JSON-file storage, OS CSPRNG, 13.8 MB standalone exe via trailer, headless/E2E/bench harness, restart-based `dev`. Evidence: `window.rs`, `session.rs`, `host.rs:14-25,160-308,674-751`, `bundle.rs:13-16,168-194`.

### 2.2 Partially implemented (works with known gaps)

| Area | What works | Gap | Primary refs |
|---|---|---|---|
| Dynamic `import()` | Loader + specifier rewrite exists | Zero dedicated tests | `build.mjs:123-135`, `quickjs.rs:44-49` |
| Dev loader `from_directory` | `.js/.mjs` only | No TS/JSX/bare-specifier parity with build | `bundle.rs:100-143` vs `build.mjs` |
| Type-check parity | TS gated, JS accidental via `tsconfig` | No explicit `allowJs/checkJs` contract | `build.mjs:146-159` |
| Fetch | Assets/data:/http, headers/Blob | No streaming, FormData, cookies, CORS, cache; 64 MiB cap | `host.rs:250-308`, `web.js:273-367` |
| Images | PNG tested | JPEG/GIF/WebP decoded but untested; no SVG/animation | `host.rs:583-586` |
| Forms | Typing/caret/selection/clipboard/change/submit | No undo, validation UI, select popup, file/date/range inputs | `session.rs:529-605` |
| `getComputedStyle`/geometry/`ResizeObserver` | Fixed property list, one content-box size/frame | Incomplete lists, no box options | `host.rs:521-567`, `web.js:428-447` |
| Restyle fast path | Attr/class/inline + hover/focus incremental | Viewport change forces `all=true` full restyle (~22.7 ms) | `style.rs:96-139`, `lib.rs:301` |
| Box generation | Correct output, Taffy input diffed | Full `build_root` rebuild every relayout (~5 ms/9k nodes) | `boxtree.rs`, `lib.rs:326-342` |
| Text cache | Paragraph cache + width-sharing | 1/64 px key granularity, full clear at 2048 cap | `text.rs:221-242` |
| Atlas | Persistent + eviction counter | Full-clear eviction, one-frame stale risk | `renderer.rs:299-333` |
| Renderer fidelity | SDF rects/borders/gradients≤4/shadows/clips/transforms | No offscreen group opacity, per-side border single colour, no filter/dirty-rect | `renderer.rs:619-632` |
| Packaging helpers | `signing.mjs`, `installer/nsis.mjs` exist | Unwired from current CLI; no sign/install/update flags | `bin/rendrix.mjs` vs `bin/rendrix-legacy.mjs:531-569` |

### 2.3 Missing (verified absent, not assumed)

Single-JS-context per process; multi-window; native dialogs; drag-drop payload (`window.rs:331` discards); tray; global shortcuts; updater; power-blockers; external links; Canvas 2D/WebGL/WebGPU (`getContext→null`); WebSocket; XHR; Workers; ServiceWorkers; IndexedDB; Notifications; Geolocation; Shadow DOM; custom elements (`NotSupportedError`); tables/floats/multi-column; `:has()`; 3D/perspective; `crypto.subtle`; `Intl` locale data; `WebAssembly`; `Temporal`; process/pty; accessibility tree (no `accesskit` dep; `ACCESSIBILITY_PLAN.md:5`); source maps (`sourceMap:false`); state-preserving hot reload; devtools/inspector; framework adapters; macOS/Linux validation (same code, unbuilt/unrun); signing/installer wiring; CI runs; allocation/Criterion profiling.

## 3. Subagent findings (reconciliation)

- **Subagent 1 (runtime/language):** JS and TS already share one transpile path; parity gap is contractual (type-check config, dev-loader divergence, untested dynamic import), not engine capability. Recommends formalising "same pipeline, conditional check", unifying or scoping the dev loader, dynamic-import tests, keeping `rquickjs` + `typescript` (build-only), deferring `Val` widening until profiles demand it, adding an interrupt-based runaway-script guard, and adding JS/linker micro-benchmarks first.
- **Subagent 2 (rendering/graphics):** Resize 36.12 ms decomposes by code location: "style" 22.72 ms = `refresh_rules` reparse + full-tree `restyle(all=true)` with per-node `Arc`/`BTreeMap` churn; "layout" 11.62 ms = whole-tree `build_root` (~5 ms, same as mutation frames where style ≈ 0.05 ms) + `TaffyEngine::sync` diffing + `measure→paragraph` shaping misses on width change. Hover (1.03 ms) and scroll (1.16 ms) prove the incremental restyle/paint path works — resize simply never uses it. Recommends measurement harness first, then stylesheet/RuleSet cache, viewport-dependency pruning, incremental boxes, text-cache fast paths, allocation discipline, GPU hygiene, scheduler micro-fixes. Explicitly retain Taffy/wgpu/cosmic-text.
- **Subagent 3 (platform/application):** Single-window/single-context is the hardest IDE/browser blocker, followed by no terminal/process model, no Canvas 2D, no workers/WebSocket/streaming, full box rebuild at editor scale, no a11y tree, no inspector/maps/HMR/adapters, and unwired distribution. Recommends incremental boxes first (shared with Subagent 2), then multi-window, dialogs/drag-drop, Canvas 2D, dedicated workers, WebSocket client, AccessKit, fetch scoping, deferred KV/pty, wired signing/update + macOS/Linux, DX ordering maps→HMR→inspector→adapters.

## 4. Conflict resolution

No load-bearing conflicts. Three apparent tensions resolved:

1. **New dependencies (Subagent 3: `rfd`, `tungstenite`, `accesskit`) vs "no unnecessary dependencies".** Resolution: allow exactly these three narrow, single-purpose, well-maintained crates when their phase arrives (`rfd` for dialogs, `tungstenite`+existing TLS for WebSocket, `accesskit`+`accesskit_winit` for a11y). Each replaces hand-rolled per-OS code that would be larger and buggier. Everything else (layout, shaping, GPU, HTTP, images, clipboard, KV unless proven) stays as-is. `redb/sled` for storage and any canvas/media/3D crates are deferred until a consuming app-class demands them — not approved now.
2. **Workers/threads vs single-context simplicity.** Resolution: keep the main frame loop single-context; add at most one dedicated-worker model (own `JsRuntime` per worker thread, `postMessage` over existing `structuredClone`, no `SharedArrayBuffer` in v1, no DOM access from workers). This preserves determinism of `Session::frame` while unblocking IDE background work.
3. **Fetch scope.** Resolution: do not retrofit browser CORS/navigation/ServiceWorkers. Either add streaming + `FormData` + cookie jar and document the native-app subset (no CORS/cache), or explicitly scope Rendrix as non-browser. The current half-state (full buffering, `Body.body===null`, 64 MiB cap) must not stand.

## 5. Shared architectural dependencies (the real ordering constraints)

1. **Incremental box generation** is the root dependency. It unblocks mutation performance, media-restyle pruning (R1.2 needs subtree patching), HMR/DOM preservation, framework adapters, and editor-scale documents. Dirty-tracking (`style_roots`/`state_roots`/`detached`) already exists — the gap is purely box-tree persistence/splicing.
2. **Stylesheet/RuleSet caching + dependency flags** unblock resize performance independently of boxes but compose with them (pruned cascade → patched boxes → partial Taffy).
3. **TextSystem paragraph cache** sits on the layout↔paint boundary; key quantisation and LRU affect both box/measure cost and resize storms. Change keys/cache policy once, benefit both.
4. **`Session::frame` scheduler** (`timers → fetches → rAF → update → after_layout → paint → present`) is shared by JS timers, WebSocket/worker message delivery, multi-window wakeups, and a11y diffing. Per-window scheduling and per-frame message caps must be designed together to preserve idle-zero-work.
5. **Bundle format `rendrix-app/1`** couples CLI build and runtime loader. Dev-loader parity, source maps, and HMR all touch this contract — version it explicitly if fields are added.
6. **Permission model** (`fs_read/fs_write`, network flag) couples fs/fetch/clipboard and gates future dialogs/terminal/workers. Narrow, explicit, user-visible grants; no general `child_process` pass-through.
7. **Measurement harness** (Criterion micro-benches + allocation counters + existing `bench 1000` medians) gates every perf claim. No optimisation without before/after on the same Windows/RTX-3060/Vulkan setup plus a second-machine sanity check.

## 6. Build vs retain decisions

| Capability | Decision | Rationale |
|---|---|---|
| QuickJS (`rquickjs =0.14.0`) | Retain behind `JsEngine` trait | ES2023+TLA+modules at 13.8 MB; JS not the measured bottleneck; swap only on proof |
| TypeScript compiler API (npm) | Retain, build-only | Runtime never sees TS; no Rust transpiler (`swc`) — would duplicate + bloat binary |
| Taffy 0.9 | Retain | Already incremental; box generation (not Taffy solve) is the cost |
| wgpu 25 | Retain | 0.3–0.6 ms GPU; single-pipeline batching sound |
| cosmic-text 0.14 | Retain | Do not rewrite shaping; fix cache keys/policy only; optionally cache font DB on disk |
| winit 0.30 | Retain, extend to N windows | Multi-window is a data-model change, not a windowing-crate change |
| `image`, `ureq`, `arboard`, `serde` | Retain | Mature; trim `image` features only if size demands (would drop declared JPEG/GIF/WebP) |
| `rfd` (dialogs) | Add when R-P2 arrives | Tiny sync dialogs; legacy code already used this approach |
| `tungstenite` (WebSocket) | Add when R-P3 arrives | Minimal framing client; reuse TLS posture; no hand-rolled protocol |
| `accesskit` + `accesskit_winit` | Add when R-A1 arrives | Only viable OS-AT bridge (UIA/NSAccessibility/AT-SPI) |
| Criterion | Add as dev-dependency | Measurement only, never shipped |
| `redb`/`sled`, media/3D crates, V8, `swc` | Do not add now | Deferred until a committed app-class requires them |

## 7. Performance analysis (verified numbers only)

`bench 1000` medians (`PERFORMANCE_AUDIT.md:18-29`): idle 1.09, hover 1.03, scroll 1.16, class-toggle 6.92 (style 0.06 + layout 5.25), text-change 8.42 (0.01 + 6.28), add/remove 7.09 (0.03 + 5.26), JS-toggle 7.57, resize 36.12 (style 22.72 + layout 11.62 + paint 0.19 + GPU 0.57), startup 607 ms + 819 ms first frame. `ENGINE_ASSESSMENT.md:138-152` reports the same shape (resize 39 = 24.5 + 12.4; run-to-run tenths).

Bottleneck classification (all three subagents agree):

- **Implementation-level:** whole-tree `build_root` (~5 ms), full restyle on viewport change (reparse + per-node clones), 1/64 px paragraph-key misses on resize, per-frame allocations (`tag.to_string`, `kids`/`items` vecs, class splits, `BTreeMap` clones, `depth_first` vecs), atlas full-clear eviction, per-frame bind-group creation.
- **Architectural (irreducible floor):** viewport-relative units/percent/wrapping legitimately change used values on resize; media queries must re-resolve; cold start pays one system-font scan + first shaping. Dependency-aware pruning lowers the ceiling toward this floor but cannot reach zero.
- **Not bottlenecks:** GPU submission, vsync pacing (correctly excluded), Taffy solve itself, JS execution/microtasks.

Targets (to be validated, not claimed): mutations < 3 ms (120 FPS), resize < 15 ms, first frame < 0.2 s after font-cache work, hover/scroll ≤ 1.5 ms preserved, idle zero-work preserved, two-window idle ≈ one-window idle.

## 8. Risks

- Optimising without the R0 harness regresses the 1 ms hover/scroll path. Mitigation: equivalence tests (patched tree `==` full tree, pruned restyle `==` full restyle, screenshot diffs) + bench-after-each-step.
- `InlineContent::key` rehashing, `BTreeMap` custom-prop clones, and `depth_first` scans look cheap singly but dominate at 9k nodes — allocation counters must be part of R0.
- Multi-window and workers both touch `Session`/scheduler lifetime assumptions (`strongWrappers`, `FinalizationRegistry`, `tick`/`frame`); design the ownership/message-bridge once, not per feature.
- `Val` boundary (`Undefined/Null/Bool/Num/Str/List` + JSON/base64 for objects) is correct and fast enough today; widening it prematurely adds borrow/unsafe risk for unmeasured gain. Batch getters first if profiles ever implicate it.
- Distribution (signing/install/update, macOS/Linux) is process work, not engine work — it blocks "platform" claims without touching frame budgets. Do not let it starve R0–R2.

## 9. Implementation-mission outcomes (2026-11-14)

First mission (5 subagents + integration): 19/24 tasks, 202 Rust + 13 CLI green, resize 38.85 -> 11.54 ms. Follow-up mission (3 subagents + integration): remaining A3/A4/D2-D4 landed — all 24 plan tasks done. Final: cargo test --workspace 234 passed / 0 failed / 2 ignored (documented follow-ups), CLI 16/16, Solid adapter 2/2, release bench 1000 resize 10.07 ms, mutations ~6.5-7.0 ms, idle/hover/scroll unchanged, display list identical (119/1/773). New deps total (approved only): rfd, tungstenite, rustls, accesskit, accesskit_winit, criterion (dev-only), solid-js@1.9.19 (adapter-scoped). No V8/swc/3D/media rewrites; Taffy/wgpu/cosmic-text retained. E2E-verified on Windows 11 / RTX 3060 / Vulkan: canvas pixels, worker echo, --inspect snapshot, hot-reload session tests, Solid matrix, existing TS suite.

