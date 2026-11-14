# Rendrix Implementation Plan

Date: 2026-11-14. Source: `ARCHITECTURE_REVIEW.md` (three subagent investigations + four audits). Status labels: ✅ implemented (keep), 🟡 partially implemented (extend), ❌ missing (build). Every item lists justification, crates/files, build-vs-retain, perf implications, tests, and dependencies. No application code was modified to produce this plan. All perf targets are goals to be measured with the Phase-0 harness on the reference setup (Windows 11, RTX 3060, Vulkan) — not claimed results.

## Ordering summary

| Order | ID | Task | Depends on | Impact |
|---:|---|---|---|---|
| 0 | M0 | Criterion + allocation harness | — | Gates all perf claims |
| 1 | P1 | Incremental box generation | M0 | Mutations 7 ms → <3 ms |
| 2 | P2 | Stylesheet/RuleSet cache | M0 | Cuts reparse from resize |
| 3 | P3 | Viewport-dependency restyle pruning | P1, P2 | Resize 36 ms → <15 ms |
| 4 | P4 | Text cache keys + LRU | M0 | Resize/shaping storms |
| 5 | P5 | Allocation discipline | M0 | Constant-factor, amplifies P1–P4 |
| 6 | L1 | JS/TS parity contract | — | No .js/.ts drift |
| 7 | L2 | Dynamic-import tests | — | Correctness |
| 8 | L3 | Dev-loader parity scope | L1 | Dev/build consistency |
| 9 | L4 | Runaway-script guard | M0 | Robustness |
| 10 | G1 | GPU hygiene | M0 | Stale-glyph + driver work |
| 11 | G2 | Scheduler micro-fixes | M0 | Pointer/id paths at scale |
| 12 | A1 | Multi-window | P1 | IDEs/browsers/multi-doc |
| 13 | A2 | Dialogs + drag-drop payload | — (+`rfd`) | File apps |
| 14 | A3 | Canvas 2D on wgpu | P1 | Editors/plots/terminal |
| 15 | A4 | Dedicated workers | L1, P1 | IDE background work |
| 16 | A5 | WebSocket client | — (+`tungstenite`) | LSP/collab |
| 17 | A6 | Fetch scoping (streaming/FormData/jar or explicit non-browser) | — | Large downloads |
| 18 | A7 | Accessibility tree | P1 | Compliance/distribution |
| 19 | D1 | Source maps | L1 | Debuggability |
| 20 | D2 | State-preserving reload | P1, D1 | DX |
| 21 | D3 | Inspector overlay | P1 | DX |
| 22 | D4 | Framework adapters (Solid first) | P1, L1 | Ecosystem |
| 23 | S1 | Signing/install/update wiring + macOS/Linux validation | — | Shipping |
| 24 | — | Deferred: KV/pty/3D/media/multi-process | Explicit | Do not build now |

Retain decisions (no work, recorded so they are not re-litigated): `rquickjs =0.14.0` behind `JsEngine` trait; `typescript` npm build-only; Taffy 0.9; wgpu 25; cosmic-text 0.14; winit 0.30; `image`/`ureq`/`arboard`/`serde`. See `ARCHITECTURE_REVIEW.md` §6.

## Completion status (implementation mission, 2026-11-14, main-agent verified;
## A3/A4/D2–D4 mission — see table rows A3, A4, D2, D3, D4)

`cargo test --workspace`: 234 passed, 0 failed, 2 ignored (Session-level follow-up
variants; engine/renderer-level coverage landed). `node --test packages/cli/test`:
16/16, `packages/solid-adapter/test`: 2/2. Release `bench 1000` integrated table in `PERFORMANCE_AUDIT.md`.

| ID | Status | Evidence |
|---|---|---|
| M0 | Done | 6 crate bench targets + Node bench + counting allocators + `bench.rs` M0 columns; HEAD baseline + CSS medians recorded |
| P1 | Done | Per-NodeId fragment pool + patch/splice in `boxtree.rs`; 13-op fuzz green; resize layout −17%, mutations −20–40% |
| P2 | Done | `parse_cached` + `ua_sheet`; reparse eliminated on media-only rebuilds |
| P3 | Done | `has_media_rules`/`uses_viewport_units`/`uses_percent` + media-only `all=false`; resize restyles 1.0 → 0.0, style 25.17 → 0.40 ms |
| P4 | Done | 0.5 px buckets, single-entry LRU, caret-lookup reuse; `hit_rate`/`cache_len` surfaced |
| P5 | Done | Two-phase custom-prop scan, cached class splits, scratch buffers, no `tag.to_string`/`kids` vecs |
| L1 | Done | `typeCheck`/`buildApp` contract + `--check-js` CLI + template `allowJs/checkJs` + parity tests |
| L2 | Done | CLI rewrite + `rendrix-js` execution tests (static/dynamic/TLA) |
| L3 | Done | `from_directory` dev-JS-only + actionable TS error (no Rust TS compiler, as decided) |
| L4 | Done | `set_execution_deadline` interrupt + 5 s task budget + rewrite; infinite-loop/microtask tests |
| G1 | Done | Cached globals bind group, headroom regrow, generation-tagged atlas + pixel-stability test |
| G2 | Done (owned side) | Caret single-lookup, budgeted `take_up_to` helper (unwired — needs pending-upload wake; documented) |
| A1 | Done | Multi-window `HashMap<WindowId>` + per-window Session/vsync + `window.open` handles + isolation tests |
| A2 | Done | `rfd` dialogs + `openExternal` + drag-drop `DragEvent` + mock seam + tests |
| A3 | Done | Offscreen per-canvas GPU targets + CPU rasterizer (paths/fill/stroke/transforms/2-stop gradients/drawImage/get-putImageData/toDataURL-PNG) composited via display-list `Image` items; invalidation test (no sibling restyle/relayout); 16 gfx + 3 JS tests green; e2e pixel asserts pass in real headless app; unsupported list documented |
| A4 | Done | Per-thread `JsRuntime` + mpsc bridge, prelude `Worker`, 16-msg/frame pump cap in `Session::frame`, 500 ms bounded `Drop` shutdown; 10 worker tests green (echo/structuredClone/errors/50-cycle/no-DOM/flood); e2e echo passes in real headless app; no SharedArrayBuffer/DOM/nested workers (documented) |
| A5 | Done | `tungstenite` client + prelude class + 16-msg/frame cap + echo/close-code tests |
| A6 | Done (scoped) | Cookie jar + `FormData`/multipart + `File`; streaming explicitly scoped out (native single-origin decision, documented) |
| A7 | Done | AccessKit tree builder + `accesskit_winit` bridge + `matchMedia` prefs + snapshot tests |
| D1 | Done | `sourceMaps` in bundle + `sourcemap.rs` VLQ remap + session load + CLI tests |
| D2 | Done | `hot_reload.rs` validate-in-scratch + commit-on-live-Engine design; CLI diffs modules + writes sidecar; runtime polls `RENDRIX_HOT_FILE` in `about_to_wait`; `--no-hot` fallback; 3 dx + 3 CLI tests green; failed updates keep last-good |
| D3 | Done | `inspector.rs` on-demand JSON snapshot (DOM/styles/layout/stats/console/errors); `--inspect` headless flag; 0 per-frame cost; no overlay UI (documented) |
| D4 | Done | `packages/solid-adapter` on upstream `solid-js@1.9.19` (adapter-scoped): mount/update/unmount/cleanup/events/`Show`/`For`; 13-step headless matrix green; limitations documented (shim, accessor rules, no SSR, pinned version) |
| S1 | Done (Windows-verified) | `--sign/--installer/--manifest/--publish-url/--app-version`, macOS `.app` layout, NSIS fix, update client w/ hash verify; macOS/Linux runs still unvalidated (no claims) |

Deferred as decided (not blockers): KV/`redb`/`sled`, pty/terminal, 3D, media decode, multi-process, `Val` widening, `InlineContent::key` memo, id-map, dedicated `__rendrix_wsDone`/`dialogDone` hooks, core-owned a11y tree (host-side equivalent tested).

---

## Phase M0 — Measurement (do first, blocks all perf claims)

### M0. Criterion micro-benches + allocation counters (❌ missing)
- **Technical justification:** `bench 1000` conflates reparse/restyle/shaping/Taffy-solve into two columns (`EngineStats{last_style,last_layout}` in `rendrix-core/src/lib.rs`). Blind optimisation risks regressing the 1 ms hover/scroll path. `PERFORMANCE_AUDIT.md:71-77` already prescribes this.
- **Crates/files:** `engine/crates/*/benches/` (new), `engine/crates/rendrix-runtime/src/bench.rs`, `rendrix-gfx/src/text.rs:shape_count`, `rendrix-layout/src/taffy_engine.rs:30-32` stats, `rendrix-gfx/src/renderer.rs:Atlas.evictions`.
- **Build vs retain:** Build harness in-repo. Add `criterion` as dev-dependency only; never shipped. Retain all render deps.
- **Performance implications:** Zero runtime change; enables before/after medians per step.
- **Testing requirements:** Benches: `Stylesheet::parse`, `RuleSet::add_stylesheet`, `compute_style` (single + 9k tree), `build_root`, `TaffyEngine::layout` (fixed vs varying widths), `paragraph/measure` hit vs miss, `flatten`, `call_global` + `__native` round-trip, `transpileModule` (Node side). Counters asserted in tests; timing informational in CI.
- **Dependencies:** None. Blocks P1–P5, G1–G2, L4.

## Phase P — Performance foundations

### P1. Incremental box generation (❌ missing; top priority)
- **Justification:** Whole-tree `Builder::build_root()` costs ~5 ms/9k nodes — the entire mutation budget (style is 0.01–0.06 ms in those scenarios). Both rendering and platform tracks identify this as the root blocker for editor-scale DOMs, HMR, and adapters.
- **Crates/files:** `engine/crates/rendrix-core/src/boxtree.rs` (persist per-element fragments keyed by `NodeId` + anon-id scheme, splice patched subtrees, re-run `arrange` for parent context only, preserve `abs_stack` re-hang), `engine/crates/rendrix-core/src/lib.rs` (`Engine::update` relayout branch, reuse `style_roots`/`detached` from `style.rs:103-123` + `lib.rs:289-300`), `engine/crates/rendrix-dom` dirty tracking. No Taffy API change — `TaffyEngine::sync` already diffs (`taffy_engine.rs:56-88`).
- **Build vs retain:** Build internally. Retain Taffy (already incremental; its input is what gets fixed).
- **Perf:** Goal mutation frame <3 ms (from 6.9–8.4 ms); 120 FPS for mutations; preserve hover/scroll ≤1.5 ms and idle zero-work.
- **Tests:** Box-tree equivalence fuzz (patched `==` full `build_root`, structural + style `Arc` equality) across insert/remove/move/class/text/`:nth-child`/abs-pos; 8 layout + 34 core tests unmodified; `bench 1000` mutation scenarios improve with identical screenshots; 156 Rust + 9 CLI green.
- **Dependencies:** M0 (equivalence harness + bench). Blocks P3, A1, A3–A4, D2–D4, A7-diff design.

### P2. Stylesheet parse + RuleSet cache (❌ missing)
- **Justification:** `refresh_rules` (`rendrix-core/src/style.rs:42-92`) re-tokenises/re-parses every sheet on every resize even when source is unchanged. Media applicability is a filter over parsed rules, not a reason to reparse. Attacks the reparse slice of the 22.72 ms style column.
- **Crates/files:** `rendrix-core/src/style.rs:42-92`, `rendrix-dom/src/lib.rs:896-901` (`note_sheet_change` source tracking), `rendrix-css/src/parser.rs`. Parse once per content hash; rebuild `RuleSet` via `add_rules` under the new `MediaContext`; parse UA sheet once at startup.
- **Build vs retain:** Build internally (use `std` hashing). No parser replacement.
- **Perf:** Removes O(stylesheet-bytes × selectors) reparse per resize; remainder = rule filtering + restyle. No behaviour change.
- **Tests:** Parser tests unchanged + new: resize with static sheets yields same `RuleSet.len()` and identical computed styles; cross-breakpoint resize flips media rules.
- **Dependencies:** M0. Feeds P3.

### P3. Viewport-dependency restyle pruning (🟡 partial → incremental)
- **Justification:** `all = all_style‖rules_rebuilt‖viewport_changed` (`lib.rs:301`) forces full `depth_first` restyle on every resize, yet bench CSS has zero `@media`. Percent/viewport-unit/wrap subtrees must still update (used values depend on viewport via `StyleContext{viewport_w/h}` + `dim(Percent)` + `measure(max_width)`), so pruning is by *dependency*, not "media rules only".
- **Crates/files:** `rendrix-css/src/{parser,cascade,properties}.rs` (per-ruleset `has_media_rules`, per-selector/decl viewport-dependency flags for `vw/vh/vmin/vmax/%`), `rendrix-core/src/style.rs:96-139`, `lib.rs:301`.
- **Build vs retain:** Build internally. Cascade/matching engine untouched.
- **Perf:** Phase 1 (safe): if `!has_media_rules && !viewport_unit_or_percent_used`, skip `compute_style` entirely on resize (layout still reruns since widths change) — removes most of 22.72 ms on bench-like pages. Phase 2: fine-grained media-dependent roots; media-heavy pages fall back to today's path (no regression). Goal resize <15 ms combined with P1/P4.
- **Tests:** Restyle-equivalence (pruned `==` full `styles` vecs), breakpoint-flip, custom-prop inheritance preserved.
- **Dependencies:** M0, P1 (subtree patching), P2 (media-outcome compare).

### P4. Text measure/shape fast paths (🟡 partial)
- **Justification:** Resize 1000→960 misses the paragraph cache for nearly every width (`width_bits=(w*64).round()+1`, `text.rs:232`) and `InlineContent::key()` re-hashes full text per sync (`rendrix-layout/src/lib.rs:39-61`). cosmic-text itself is retained — only keys/policy change.
- **Crates/files:** `rendrix-gfx/src/text.rs:37-62,221-242` (coarsen quantisation to 1 px/0.5 px, keep single-line width-sharing, LRU instead of full `clear()` at 2048), `rendrix-layout/src/lib.rs:37-62` (memoize `key()` per content `Arc`), `taffy_engine.rs` measure closure.
- **Build vs retain:** Build internally (hand-rolled small LRU/clock; no new crate). Retain cosmic-text.
- **Perf:** Fewer `shape_until_scroll` calls on resize/scroll, lower per-frame hashing; no visual change (widths still `ceil`ed at paint).
- **Tests:** `cache_reuses_shaping` extended (quantised widths share entries), wrap-correctness at coarse boundaries, resize screenshot diffs, `shape_count` hit-rate asserted.
- **Dependencies:** M0.

### P5. Per-frame allocation discipline (🟡 partial, no API change)
- **Justification:** Hot loops allocate per node: `tag.to_string()` (`boxtree.rs:96`), `kids/collect/items` vecs, `collapse` strings, per-element class `split_whitespace` inside `matching()` (`cascade.rs:136-140`), attr `to_string/clone`, full `BTreeMap` custom-prop clones, `depth_first → Vec` scans. At 9k nodes the constant factors are the ~5 ms.
- **Crates/files:** `rendrix-core/src/boxtree.rs`, `rendrix-css/src/cascade.rs:128-159,198-211` (hoist class-split per `compute_style`, skip `BTreeMap` clone when `any_custom` false), `rendrix-dom/src/lib.rs` `depth_first` call sites, `rendrix-layout/src/taffy_engine.rs:45-88` (scratch buffers).
- **Build vs retain:** Build internally (pooled/`thread_local` scratch vecs, tag comparison without allocation). No API change.
- **Perf:** Lowers mutation/resize constant factor even before P1 lands; less allocator/GC pressure.
- **Tests:** Existing suites + bench allocation counters; screenshot equivalence.
- **Dependencies:** M0. Amplifies P1–P4.

## Phase L — Language parity + robustness

### L1. Formalise "same pipeline, conditional check" for JS/TS (🟡 partial)
- **Justification:** `transpileModule` already handles both (`build.mjs:205-214`), but JS exemption is accidental (depends on project `tsconfig` `allowJs/checkJs`). Prevents `.js`/`.ts` drift while keeping one bundle path.
- **Crates/files:** `packages/cli/src/app/build.mjs:146-159,180-216`, template `tsconfig.json` (via `commands.mjs:init`).
- **Build vs retain:** Build internally. Retain `typescript ^5.9.3` (build-only, never shipped). Explicit overrides: `allowJs:true, checkJs:false` by default with `--check-js` opt-in; document "JS skips type-check, never skips transpile/bundle".
- **Perf:** Zero runtime cost; same per-file transpile.
- **Tests:** CLI: identical `.js`/`.ts` inputs → identical `.js` output (modulo types); dirty TS fails `typecheck`, dirty JS passes unless `--check-js`.
- **Dependencies:** None.

### L2. Dynamic-`import()` rewrite + execution tests (🟡 partial)
- **Justification:** `collectSpecifiers` already visits `ImportKeyword` (`build.mjs:130`) and the QuickJS loader serves it, but zero tests cover the rewrite — runtime-only failures lurk for code-split patterns.
- **Crates/files:** `packages/cli/src/app/build.mjs:123-135,186-201`, `engine/crates/rendrix-js/src/quickjs.rs:44-49`, `tests.rs:240-253` as template.
- **Build vs retain:** Build internally (tests only unless a bug surfaces). No dep change.
- **Perf:** Validates no extra loader cost.
- **Tests:** CLI test for `import('./chunk.ts')` + variable-specifier passthrough; `rendrix-js` test for dynamic import + TLA ordering.
- **Dependencies:** None. Blocks any code-splitting claim.

### L3. Dev-loader parity scope (🟡 partial)
- **Justification:** `AppBundle::from_directory` (`bundle.rs:100-143`) handles only `.js/.mjs` while production `buildApp` handles TS/JSX/bare-specifiers/CSS/JSON — parity violation lives in the loader, not the engine.
- **Crates/files:** `engine/crates/rendrix-runtime/src/bundle.rs:100-143,253-269` vs `packages/cli/src/app/build.mjs`.
- **Build vs retain:** Build internally. Short-term: make `from_directory` explicitly dev-JS-only with an actionable error ("run `rendrix dev/build` for TS/TSX/bare imports"). Longer-term only by reusing the CLI — no Rust TS compiler (`swc`) in the binary.
- **Perf:** No runtime impact.
- **Tests:** `bundle.rs` tests assert TS file in dir either errors actionably or loads post-build.
- **Dependencies:** L1 (contract wording).

### L4. Runaway-script guard (❌ missing)
- **Justification:** Only missing Phase-1 robustness item: infinite loop/TLA stall blocks `execute_pending_job`/`frame` with no preemption; 1 GB/2 MB limits bound memory/stack only.
- **Crates/files:** `engine/crates/rendrix-js/src/quickjs.rs:122-150` (interrupt/deadline), `runtime.rs:92-104`, `session.rs:634-682`.
- **Build vs retain:** Build internally via `rquickjs` interrupt handler checked in `tick`/`frame`; convert to `JsError` + `reportError`. No new engine crate.
- **Perf:** Negligible (one deadline check per job/timer).
- **Tests:** Infinite-loop script terminates with error inside budget; pending-TLA module still `WouldBlock`-ok.
- **Dependencies:** M0 (budget harness).

### L5/L6. Retain decisions (✅ keep, no work)
- **L5 Keep `rquickjs =0.14.0`, no V8.** Justification: behind `JsEngine` trait; 13.8 MB single binary; JS not the measured bottleneck (`PERFORMANCE_AUDIT.md:40-44`). Files: `engine/Cargo.toml:25`, `quickjs.rs`, `engine_trait.rs`. Tests: existing 27 JS tests.
- **L6 Keep `typescript` npm, no runtime TS.** Justification: runtime must never see TS; Rust transpiler would duplicate the compiler API and bloat the binary. Build-only dep, not shipped.

## Phase G — Rendering hygiene (small, risk-averse; GPU is not the bottleneck)

### G1. GPU/upload/present hygiene (🟡 partial)
- **Justification:** GPU is 0.31–0.75 ms — changes must not risk the one-draw-call batching or premultiplied-alpha contract.
- **Crates/files:** `engine/crates/rendrix-gfx/src/renderer.rs:195-236,299-363` (reuse globals `BindGroup` instead of per-frame `create_bind_group`; grow `inst_cap` with headroom; generation-tagged atlas re-upload or optional larger atlas, default 2048 unchanged).
- **Build vs retain:** Build internally. Retain wgpu + shader architecture (`shader.wgsl`).
- **Perf:** Trims CPU-side driver work; removes eviction-stale-frame artefact; no fidelity change.
- **Tests:** 12 gfx GPU pixel tests + `render_stats` batch/instance assertions; forced-overflow recovery test.
- **Dependencies:** M0.

### G2. Scheduler/input-path micro-fixes (🟡 partial)
- **Justification:** Two latent O(n)/redundant costs on the interactive path, cheap once M0 proves them.
- **Crates/files:** `engine/crates/rendrix-dom/src/lib.rs:784-848` (incremental hover/active/focus chain diff instead of `depth_first(root)` scans), `engine/crates/rendrix-runtime/src/session.rs:639-681` (avoid double-`update` unless same-frame geometry required), `rendrix-dom/src/query.rs:119-121` (id map for `get_element_by_id` instead of linear scan).
- **Build vs retain:** Build internally. Behaviour unchanged.
- **Perf:** Pointer-move/key paths stay ≤1 ms at larger trees; removes second-pipeline risk on resize-drag storms.
- **Tests:** Session input tests + e2e hover/click steps; id-map consistency on moves/removes.
- **Dependencies:** M0.

## Phase A — Application platform (ordered by app-class blocking severity)

### A1. Multi-window, single process (❌ missing; hardest IDE/browser blocker)
- **Justification:** `struct App { gpu, session }` + one `Window`/`Surface` (`window.rs:23,38-127`) plus `ARCHITECTURE.md:90` single-window/single-context cap blocks IDEs, browsers, any multi-document app (`window.open()=>null`, `web.js:673`).
- **Crates/files:** `rendrix-runtime/src/window.rs` (`App` → `HashMap<WindowId,(Gpu,Session)>`, per-window vsync/`WaitUntil`), `session.rs` (one `Session` per window; recommend separate engine+JS context per window first for isolation, shared clipboard), `host.rs`/`prelude/web.js` (`window.open/close/resize/setTitle` with handles).
- **Build vs retain:** Build on retained `winit 0.30` (already supports N windows). No windowing framework.
- **Perf:** Per-window scheduling; idle windows do zero work. Goal: 2-window idle ≈ 1-window idle.
- **Tests:** Headless two-session test + scripted `window.open/close` e2e; per-window focus semantics.
- **Dependencies:** P1 (per-window subtree cost must be incremental first).

### A2. Native dialogs + drag-drop payload (❌ missing; smallest file-app unblock)
- **Justification:** `fs_*` requires prelisted dirs (`host.rs:310-317`); real apps need user-picked files. `window.rs:331` discards `DroppedFile/HoveredFile`.
- **Crates/files:** `host.rs` (new `showOpenDialog/showSaveDialog/openExternal` ops), `prelude/web.js` (`rendrix.dialog.*`), `window.rs:331` (deliver paths/bytes).
- **Build vs retain:** Add `rfd` to `rendrix-runtime` only (tiny sync dialogs; matches legacy approach). No hand-rolled per-OS dialogs.
- **Perf:** Modal by OS design; document timer behaviour.
- **Tests:** Headless mock-op injection + manual Windows/macOS/Linux matrix.
- **Dependencies:** S1 for non-Windows validation.

### A3. Canvas 2D on the existing wgpu renderer (❌ missing)
- **Justification:** `getContext→null` (`dom.js:574`) is a hard wall for editors, plots, terminal renderers, and any `<canvas>`-expecting port. SDF pipeline is retained and fast — Canvas becomes one more textured-quad producer.
- **Crates/files:** `rendrix-gfx` (new per-`<canvas>` offscreen texture target), `prelude/dom.js` (`HTMLCanvasElement` + `CanvasRenderingContext2D` subset), `host.rs` (path/fill/stroke/text/image ops), display-list compositing.
- **Build vs retain:** Build internally; reuse cosmic-text shaping, `image` decoder, SDF pipeline. No external canvas lib.
- **Perf:** Per-canvas texture-upload budget with dirty-region re-upload. Goal: 1000×700 fill <2 ms incremental (to be validated).
- **Tests:** Pixel tests per op + JS API tests in the style of `tests.rs`.
- **Dependencies:** P1 (layout cost of canvas sizing).

### A4. Dedicated workers, subset (❌ missing)
- **Justification:** Single QuickJS context on the main pump (`runtime.rs:161-183`) means parse/compile/format blocks frames. Dedicated workers with `postMessage` + existing `structuredClone` (`web.js:494-509`) suffice; full ServiceWorkers/shared memory is overkill.
- **Crates/files:** `rendrix-js/src/{runtime,host,quickjs}.rs` (per-worker `JsRuntime` on own thread, bridge in `Session::pump/frame`), `prelude/web.js` (`Worker` class).
- **Build vs retain:** Build internally on OS threads (already used for fetch). No `SharedArrayBuffer` in v1; no DOM access from workers (throws).
- **Perf:** Worker must never stall frame loop; assert frame p95 with busy worker via M0 harness.
- **Tests:** Round-trip, error propagation, termination, no-DOM-access assertion.
- **Dependencies:** L1, P1.

### A5. WebSocket client (❌ missing)
- **Justification:** `fetch` request/response cannot do LSP/devtools-protocol streaming or collab; IDE/browsers need persistent sockets.
- **Crates/files:** `host.rs` (`ws_connect/send/close`), `prelude/web.js` (`WebSocket` OPEN/message/error/close), `runtime.rs:tick()` polling like fetches.
- **Build vs retain:** Add `tungstenite` + `rustls` (match `ureq` TLS posture). No hand-rolled framing.
- **Perf:** Frame-budgeted delivery (cap messages/frame, spill to next `tick`).
- **Tests:** Local echo-server loopback, backpressure, close-code propagation.
- **Dependencies:** None (M0 for budget validation).

### A6. Fetch semantics: finish or explicitly scope out "browser" (🟡 partial)
- **Justification:** Current half-state (`Body.body===null` at `web.js:304`, 64 MiB full buffering at `host.rs:299`, no FormData/cookies/CORS/cache) will OOM or stall large downloads and cannot implement web security semantics.
- **Crates/files:** `host.rs:250-308` (stream reader instead of `read_to_end` for `ReadableStream`), `web.js:273-367` (streaming reader, `FormData` multipart encode, per-app cookie jar), `runtime.rs:169-183` (poll streaming fetches).
- **Build vs retain:** Build internally on retained `ureq`. Document no-CORS/no-cache as a native-app decision if the "browser" goal is dropped.
- **Perf:** Bounded per-frame fetch bytes; no full-body buffering for streamed bodies.
- **Tests:** Streamed large-asset test (spills across frames), `FormData` round-trip, cookie-jar persistence, `data:`/asset paths unchanged.
- **Dependencies:** None.

### A7. Accessibility tree via AccessKit (❌ missing; compliance blocker)
- **Justification:** Screen readers see nothing (`ACCESSIBILITY_PLAN.md:5`); enterprise/edu distribution blocked. Plan already written — execute it.
- **Crates/files:** Tree build in `rendrix-core` during `Engine::update` (role from tag/`role`, name from `aria-*`/`label`/text, state/value/selection, bounds from layout node ids), bridge via `accesskit_winit` in `window.rs`, AT actions → existing `Session` entry points, OS reduced-motion/contrast/text-scale → `matchMedia`.
- **Build vs retain:** Add `accesskit` + `accesskit_winit` only (UIA/NSAccessibility/AT-SPI). Tree construction built internally.
- **Perf:** Diff tree per frame, not full rebuild; AT spam must not force layout.
- **Tests:** Unit tree-shape tests + Narrator/NVDA/VoiceOver/Orca manual matrix; no "supported" claim before pass.
- **Dependencies:** P1 (bounds stability); S1 for macOS/Linux AT paths.

## Phase D — Developer experience (in this order: maps → reload → inspector → adapters)

### D1. Source maps (❌ missing)
- **Justification:** `build.mjs:106-109` disables maps; stacks cannot map to TS/TSX sources.
- **Crates/files:** `packages/cli/src/app/build.mjs` (enable `tsc` maps, store in bundle — bundle format version bump if needed), `rendrix-js` error reporting (remap stacks).
- **Build vs retain:** Build internally. Retain `typescript` (its map emitter).
- **Perf:** Build-time only; negligible runtime (map lookup on error path).
- **Tests:** CLI map-emission tests + stack-remap tests.
- **Dependencies:** L1.

### D2. State-preserving hot reload (❌ missing; `dev` is full restart)
- **Justification:** `commands.mjs:90-130` kills + respawns the process (120 ms debounce). Complex apps need module re-evaluation with DOM/state preserved.
- **Crates/files:** CLI dev loop + runtime module loader (`session.rs:159-167`, `quickjs.rs` module registry).
- **Build vs retain:** Build internally. No new dep.
- **Perf:** Reload must be faster than full restart and must not regress frame budgets (patched boxes from P1 reused).
- **Tests:** E2E reload preserving input focus/text content across a CSS+JS edit.
- **Dependencies:** P1 (subtree patching), D1.

### D3. In-app inspector overlay (❌ missing)
- **Justification:** Debugging a 50k-node app today is `console` + screenshots.
- **Crates/files:** `rendrix-runtime` (overlay reading engine state: DOM/CSS/layout/display-list), `rendrix-core` introspection hooks.
- **Build vs retain:** Build internally as an engine-state view. No external devtools framework.
- **Perf:** Overlay disabled by default; when enabled must not perturb measured frames (separate toggle from M0 harness).
- **Tests:** Manual + headless state-dump assertions.
- **Dependencies:** P1.

### D4. Framework adapters, Solid first (❌ missing)
- **Justification:** Solid's fine-grained DOM updates map directly onto this DOM; React needs a reconciler. Both need a DOM-host adapter plus a real integration test — not a shim layer.
- **Crates/files:** `packages/types/*`, new adapter packages, examples.
- **Build vs retain:** Build adapters internally per framework (small, explicit). No compat shim.
- **Perf:** Adapter overhead measured via M0 harness on a 1k-item update benchmark.
- **Tests:** Framework render integration tests (mount/update/unmount parity with direct-DOM baseline).
- **Dependencies:** P1 (mutation cost), L1–L2 (module semantics).

## Phase S — Shipping

### S1. Signing/install/update wiring + macOS/Linux validation (🟡/❌)
- **Justification:** Trailer pack works on Windows (13.8 MB); `signing.mjs` + `installer/nsis.mjs` are unwired (`bin/rendrix.mjs` has no sign/install flags; only `bin/rendrix-legacy.mjs:531-569` references them); CI (`ci.yml`) never run; macOS/Linux unbuilt/unrun (`COMPATIBILITY.md:9-10`). Blocks any "native application platform" claim.
- **Crates/files:** `packages/cli/src/{signing.mjs,installer/*,commands.mjs,bin/rendrix.mjs}` (reconnect flags, manifest `.sig`, publish URL), minimal update client (download + SHA-256 vs manifest, `quitAndInstall` semantic) or delete `autoUpdater` refs from `nsis.mjs:16`, `rendrix-runtime/src/{window.rs,host.rs}` platform fixes, `.github/workflows/ci.yml` on all three OSes.
- **Build vs retain:** Reuse existing helpers + OS tools (`signtool`/`codesign`/`notarytool`); add macOS app-bundle + Linux artifact paths. No new update framework unless the minimal client proves insufficient.
- **Perf:** No runtime impact; packaging-time only.
- **Tests:** Existing suites on all three platforms + packaged-app smoke run per OS; manual install/update matrix.
- **Dependencies:** None (process work; runs in parallel with P/L/G once M0 exists).

## Explicit non-goals (do not schedule)

- 3D scene graph/meshes/cameras/lighting (`ROADMAP R6.2`): separate engine program, not an iteration of the 2D SDF renderer.
- `<video>`/`<audio>` decode pipeline (`R6.3`): needs a media decoding integration later; no work now.
- Multi-process sandboxing/origin isolation/SOP/CORS enforcement/navigation/ServiceWorkers (`R6.1`): Rendrix is a single-origin `rendrix://app` runtime with an explicit permission manifest by design.
- Full IndexedDB: whole-file JSON `localStorage` stands; add embedded KV (`redb`/`sled` class) only when a committed IDE/browser workload proves the need. Not approved now.
- General `child_process`/pty: intentionally ungated process spawn breaks the permission story (`capabilities.rs` dropped process grants deliberately). Narrow terminal API only if an IDE program funds it; otherwise sidecar over WebSocket/fetch.
- `Val` widening to objects/ArrayBuffers: deferred until M0 profiles implicate the `__native` boundary; batch getters first.
- V8, `swc`, media/3D crates: explicitly rejected for now per §Retain decisions.

## Test policy (applies to every phase)

Per `ENGINE_ASSESSMENT.md:110-114`: `cargo test --workspace` (156 Rust), `node --test packages/cli/test` (9 CLI), `npm run e2e` (26 scripted steps with eye-checked screenshots), release build + real windowed run at 125% DPI. Each perf phase additionally reports `cargo run --release -p rendrix-runtime -- bench 1000` medians vs the `PERFORMANCE_AUDIT.md` baseline and updates that document instead of projecting.
