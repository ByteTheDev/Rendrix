# Rendrix Implementation Audit

Date of audit: 2026-11-14. Scope: entire repository at `C:\Users\spoil\OneDrive\Desktop\Rendrix` (branch `dev`, HEAD `d4740ef`). This document reports only capabilities verified against source code and tests. Uncommitted working-tree changes in `engine/crates/rendrix-css/src/parser.rs`, `properties.rs`, `rendrix-js/src/quickjs.rs`, `rendrix-runtime/src/session.rs` are in-progress fixes and are not counted as shipped.

## Implementation-mission addendum (2026-11-14, main-agent verified, uncommitted)

Five subagents + main-agent integration landed 19 of 24 plan tasks; full
verification in `IMPLEMENTATION_PLAN.md` (completion table) and
`PERFORMANCE_AUDIT.md` (integrated bench). A follow-up mission (3 subagents +
main-agent integration) then landed the remaining 5: Canvas 2D (A3), dedicated
workers (A4), hot reload (D2), inspector (D3), Solid adapter (D4) — all 24 plan
tasks now done. Headline deltas vs the audit above (cumulative):

- Previously implemented: incremental box generation (P1), stylesheet/RuleSet
  cache (P2), viewport-dependency restyle pruning (P3), text cache LRU +
  quantisation (P4), allocation discipline (P5), JS/TS parity contract +
  `--check-js` (L1), dynamic-import tests (L2), dev-loader TS error (L3),
  runaway-script guard (L4), GPU hygiene + atlas LRU (G1), multi-window (A1),
  native dialogs + drag-drop (A2), WebSocket client (A5), fetch cookie jar +
  FormData (A6), AccessKit tree + bridge (A7), source maps end-to-end (D1),
  signing/installer wiring + update client (S1, Windows-verified).
- Now also implemented: Canvas 2D subset (A3: CPU raster + GPU texture
  targets + `CanvasRenderingContext2D` bindings), dedicated workers (A4:
  per-thread `JsRuntime` + capped pump + bounded shutdown), hot reload (D2:
  sidecar + scratch validation + live-DOM commit), inspector snapshot +
  `--inspect` (D3), Solid adapter on upstream `solid-js` (D4).
- Tests: 156 → 234 Rust passed (0 failed, 2 documented follow-ups),
  9 → 16 CLI passed, 2/2 Solid adapter tests. New deps (all approved):
  `rfd`, `tungstenite`, `rustls`, `accesskit`, `accesskit_winit`,
  `criterion` (dev-only), `solid-js@1.9.19` (adapter-scoped).
- Performance: resize 36.1 → 10.1 ms (goal <15 met); mutations ~6.5–7.0 ms
  (<3 ms goal open — Taffy re-solve floor); hover/scroll/idle unchanged;
  canvas/workers/inspector inert when unused.
- Verified end-to-end on Windows 11 / RTX 3060 / Vulkan: canvas pixels via
  `getImageData` in a real headless app, worker echo round-trip, `--inspect`
  snapshot shape, hot-reload + failed-update session tests, Solid 13-step
  headless matrix, existing TS app suite green.

## Executive summary

1. **What Rendrix can genuinely do today.** On Windows 11 it can build, run and package a native desktop application written in TypeScript/TSX with CSS, a real DOM, flexbox/grid/block layout, CSS cascade/animations/transitions, wgpu rendering, OS input, text editing, fetch, storage and observers, in a single ~13.8 MB executable with no Chromium/Electron/WebView. Verified by 156 Rust tests, 9 CLI tests, an end-to-end scripted run (`npm run e2e`, 26 steps) and a real-window run.
2. **What is partially implemented.** Form controls (no validation UI, no select popup), positioning (sticky = relative, auto-inset absolutes), gradients (≤4 stops), images (PNG tested; JPEG/GIF/WebP decoded but untested), `calc()`, `@font-face` (local fonts only), `getComputedStyle` (fixed property list), `ResizeObserver`, layout size queries, scrolling (overlay, not draggable), media queries, transforms (2D only).
3. **What is not implemented.** Canvas 2D/WebGL/WebGPU, WebSocket, XHR, Web Workers, Service Workers, IndexedDB, Shadow DOM, custom elements, tables/floats/multi-column, `:has()`, 3D rendering, accessibility tree, file dialogs, multi-window, tray, global shortcuts, auto-update, framework adapters, source maps, hot reload, macOS/Linux validation, `crypto.subtle`, `Intl`, `WebAssembly`, `Temporal`.
4. **JavaScript.** JavaScript and TypeScript are both supported through the same build pipeline; both are transpiled to ES2022 ES modules and run in QuickJS. JavaScript is not executed as raw source by the runtime without the build step, but it does not require type-checking.
5. **External packages that should remain.** wgpu, winit, Taffy, cosmic-text, QuickJS (rquickjs), image, ureq, arboard, serde/serde_json. None should be rewritten for originality.
6. **Performance.** Measured on Windows 11 / RTX 3060 / Vulkan, 1000-item list (9009 nodes): idle 1.09 ms/frame, hover 1.03 ms, scroll 1.16 ms, DOM mutation 6.9–8.4 ms, viewport resize 36.1 ms. Startup 607 ms. Bottleneck is whole-tree box generation (~5 ms) and full restyle on viewport change (~23 ms).
7. **Highest-priority next steps.** Incremental box generation; accessibility tree (AccessKit); macOS/Linux validation; Canvas 2D; framework adapters (Solid first); hot reload + source maps; cold-start font cache.

## Crate and module inventory

| Crate | Path | Status | Notes |
|---|---|---|---|
| `rendrix-css` | `engine/crates/rendrix-css/src/*` | Implemented | Tokenizer/parser, selectors, cascade, inheritance, custom props, media queries, keyframes, timing, transforms, computed style |
| `rendrix-dom` | `engine/crates/rendrix-dom/src/*` | Implemented | Arena DOM, attrs, mutation records, queries, HTML parser/serializer, form state, dirty tracking, GC |
| `rendrix-layout` | `engine/crates/rendrix-layout/src/*` | Implemented | Neutral box tree + `LayoutEngine` trait; Taffy backend (flex/grid/block) |
| `rendrix-gfx` | `engine/crates/rendrix-gfx/src/*` | Implemented | Display list, cosmic-text shaping + paragraph cache, glyph atlas, wgpu renderer |
| `rendrix-core` | `engine/crates/rendrix-core/src/*` | Implemented | Engine pipeline, hit testing, scrolling, text editing, transitions/animations |
| `rendrix-js` | `engine/crates/rendrix-js/src/*` | Implemented | `JsEngine` trait + QuickJS, module loader, DOM/Web API prelude, event loop glue |
| `rendrix-runtime` | `engine/crates/rendrix-runtime/src/*` | Implemented | Bundle loader, Session, winit driver, headless driver, benchmark |
| CLI | `packages/cli/*` | Implemented | init/dev/build/run/check/package/doctor |
| Types | `packages/types/*` | Implemented | TS declarations and JSX types |

## Capability matrix

### JavaScript and modules

| Capability | Status | Crate/Files | Works today | Limitations | Tests |
|---|---|---|---|---|---|
| ES2023 language (classes, private fields, async/await, generators, BigInt, WeakRef, FinalizationRegistry) | ✅ | `rendrix-js/src/quickjs.rs`, QuickJS-ng | Full language via QuickJS | None beyond engine | 27 JS tests |
| ES modules, relative imports, top-level await | ✅ | `quickjs.rs` (MapResolver/MapLoader), `runtime.rs` | Static + dynamic imports | Bare specifiers resolved at build time only | CLI + session tests |
| Dynamic `import()` | 🟡 | QuickJS | Provided by QuickJS | Specifier rewriting not covered by test | None specific |
| CommonJS/require/Node built-ins | ❌ | `packages/cli/src/app/build.mjs` | Build error with explanation | Not supported by design | CLI tests assert rejection |
| TypeScript, TSX | ✅ | `packages/cli/src/app/build.mjs` (`typeCheck`, `compile`) | Type-checked + transpiled | Runtime sees JS only | CLI tests |
| Promises, microtasks, queueMicrotask, unhandled rejection | ✅ | `quickjs.rs`, `prelude/web.js`, `runtime.rs` | Full | None | Session tests |
| setTimeout/setInterval/requestAnimationFrame | ✅ | `prelude/web.js`, `host.rs`, `session.rs` | Frame-driven rAF; platform timers | None | Session tests |
| Intl, WebAssembly, Workers, Temporal | ❌ | Not defined | `toLocaleString` works without locale data | Not implemented | Runtime check test |

### DOM

| Capability | Status | Crate/Files | Works | Limitations | Tests |
|---|---|---|---|---|---|
| Node/Element/Text/Comment/Document/DocumentFragment, tree mutation, DOMException | ✅ | `rendrix-dom/src/lib.rs`, `prelude/dom.js` | Full arena DOM | None major | 22 DOM + session |
| Attributes, classList, dataset, style, innerHTML/outerHTML, insertAdjacent*, cloneNode | ✅ | same | Full | HTML parser tolerant, no foster parenting | DOM tests |
| querySelector(All), matches, closest, getElementsBy* | ✅ | `rendrix-dom/src/query.rs`, `prelude/dom.js` | Type/class/id/attribute selectors, combinators, `:not :is :where :nth-* :first/last-child :empty :root :hover :active :focus :disabled :checked` | `:has()` ❌ | DOM tests |
| Events capture/target/bubble, once, passive, signal, on* props, stopImmediatePropagation, CustomEvent | ✅ | `prelude/events.js`, `session.rs` | Full | None | Session tests |
| Mouse/Pointer/Wheel/Keyboard/Input/Focus/Submit from OS | ✅ | `window.rs`, `session.rs` | Full on Windows | Touch/gamepad ❌; pointer capture no-op | Session tests |
| Form controls | 🟡 | `prelude/dom.js`, `rendrix-dom` form state | Typing, caret, selection, clipboard, input/change, label/submit activation | No undo, no validation UI, select has no popup, file/date/range inputs ❌ | Session tests |
| getBoundingClientRect, offset*, client*, scroll*, scrollIntoView, elementFromPoint, getComputedStyle | 🟡 | `prelude/dom.js`, `host.rs` | Geometry works | getComputedStyle fixed property list | Session tests |
| MutationObserver | ✅ | `prelude/web.js`, `rendrix-dom` | childList, attributes (+filter, old value), characterData, subtree | None | DOM tests |
| ResizeObserver | 🟡 | `prelude/web.js` | One content-box size per frame | Limited | None dedicated |
| IntersectionObserver, Shadow DOM, custom elements, Range/Selection, contenteditable, drag-and-drop | ❌ | Not defined or throw NotSupportedError | Feature detection works | Not implemented | None |

### CSS

| Capability | Status | Crate/Files | Works | Limitations | Tests |
|---|---|---|---|---|---|
| Cascade: origin, specificity, !important, source order, inheritance, inherit/initial/unset, custom props, var() | ✅ | `rendrix-css/src/cascade.rs`, `properties.rs` | Full | None | 39 CSS tests |
| Media queries | ✅ | `rendrix-css/src/parser.rs`, `style.rs` | width/height/orientation/prefers-color-scheme/resolution, and/not/, | Re-evaluated on resize | CSS tests |
| Box model, box-sizing, margin/padding/border, border-radius | ✅ | `properties.rs`, `paint.rs` | Full | Per-side border colour and dashed/dotted render as one solid colour | CSS tests |
| display block/inline/inline-block/flex/inline-flex/grid/none/contents | ✅ | `properties.rs`, `boxtree.rs` | Full | table*/list-item not implemented | CSS tests |
| Flexbox | ✅ | `taffy_engine.rs` (Taffy) | All properties | None | Layout tests |
| Grid | ✅ | `taffy_engine.rs` | tracks, repeat, minmax, fr, auto-fill/fit, line/span placement, gaps | grid-template-areas ❌ | Layout tests |
| Positioning relative/absolute/fixed, z-index, stacking | 🟡 | `boxtree.rs`, `paint.rs` | Works | sticky = relative; auto-inset absolutes use container origin | Core tests |
| Overflow hidden/clip/scroll/auto, wheel scrolling, nested scrollers | ✅ | `core` scroll logic, `session.rs` | Full | Scrollbars overlay, not draggable | Session tests |
| Floats, multi-column, tables, writing-mode | ❌ | Not implemented | N/A | None | None |
| Text properties | ✅ | `rendrix-css`, `text.rs` | font family/size/weight/style, line-height, letter-spacing, alignment, decoration, transform, white-space, wrapping | text-overflow ellipsis parsed not drawn; text-shadow, word-break, line-clamp ❌; bidi/complex scripts not tested | CSS tests |
| Colours, gradients, box-shadow, opacity, background-image, object-fit | 🟡 | `color.rs`, `properties.rs`, `paint.rs` | hex/rgb/hsl/named/alpha, linear/radial ≤4 stops, outer+inset shadow, opacity, bg url, object-fit | Shadow blur analytic; opacity per item not offscreen group | CSS tests |
| 2D transform, transform-origin | ✅ | `animation.rs`, `paint.rs` | Full | No 3D/perspective | CSS tests |
| Transitions and @keyframes | ✅ | `rendrix-css/src/animation.rs`, `core/style.rs` | Animatable subset | Other properties switch discretely | CSS tests |
| calc() | 🟡 | `values.rs` | Only same-kind operands | px-only or %-only | CSS tests |
| @font-face | 🟡 | `text.rs` (load_font), `core` | Local bundled TTF/OTF/TTC | WOFF/WOFF2 ❌ | None dedicated |
| ::before/::after string content | ✅ | `boxtree.rs` | String content | attr(), counters, images ❌ | Core tests |
| filter, backdrop-filter, clip-path, mask, @container, @layer ordering, nesting, @import | ❌ | Flattened or absent | N/A | None | None |

### Web platform APIs

| Capability | Status | Files | Works | Limitations |
|---|---|---|---|---|
| URL, URLSearchParams | ✅ | `prelude/web.js` | Full | No IDNA/punycode |
| TextEncoder/TextDecoder | ✅ | `prelude/web.js` | UTF-8, latin1/ascii | None |
| AbortController/AbortSignal | ✅ | `prelude/web.js` | timeout, any | None |
| fetch, Headers, Request, Response, Blob | 🟡 | `prelude/web.js`, `host.rs` | Relative assets, data: URLs, http(s) with permission | No streaming, FormData, cookies, CORS, caching |
| localStorage, sessionStorage | ✅ | `host.rs`, `prelude/web.js` | JSON file per app | No storage events |
| crypto.getRandomValues, randomUUID | ✅ | `host.rs`, `prelude/web.js` | OS CSPRNG | crypto.subtle ❌ |
| structuredClone, atob/btoa, performance.now, matchMedia, console | ✅ | `prelude/web.js` | Full | None |
| navigator.clipboard | 🟡 | `host.rs`, `prelude/web.js` | Text only | None beyond |
| Images via img/CSS bg/new Image() | 🟡 | `host.rs` image_entry, `rendrix-gfx` | PNG tested; JPEG/GIF/WebP decoded but untested; no SVG/animation |
| location, history | 🟡 | `prelude/web.js` | In-memory | No navigation |
| Canvas 2D, WebGL, WebGPU, video/audio | ❌ | `prelude/dom.js` (getContext returns null) | Feature detection | Not implemented |
| WebSocket, XHR, Workers, Service Workers, IndexedDB, Notifications, Geolocation | ❌ | Not defined | N/A | Not implemented |
| rendrix module (window, clipboard, quit, fs) | ✅ | `prelude/web.js`, `host.rs` | fs permission-gated | None |

### Application platform

| Capability | Status | Files | Works | Limitations |
|---|---|---|---|---|
| Native window (winit), DPI, resize, title, close | ✅ Windows | `window.rs`, `session.rs` | Verified Windows | macOS/Linux untested |
| Mouse, keyboard, wheel, text input, IME commit | ✅ Windows | `window.rs`, `session.rs` | IME composition untested | None |
| System clipboard | ✅ | `session.rs` (arboard) | Text | None |
| Standalone executable | ✅ Windows | `bundle.rs`, `main.rs` | 13.8 MB, no runtime | Windows only verified |
| File dialogs, multi-window, tray, global shortcuts, auto-update, accessibility tree | ❌ | Not implemented | N/A | Planned |
| Code signing/installers | ❌ (helper scripts remain, unwired) | `packages/cli/src/installer/*`, `signing.mjs` | Not wired | None |

## DOM and rendering internals (verified execution paths)

- `Engine::update` in `engine/crates/rendrix-core/src/lib.rs` runs refresh_rules → restyle → tick_motion → box tree build (`boxtree::Builder::build_root`) → Taffy layout → paint (display list). Verified by tests and bench.
- Box tree is rebuilt in full on any style/structure change (known limit, ~5 ms / 9000 nodes).
- Display list flattened into one instance buffer; usually one draw call (`renderer.rs`).
- Glyph atlas 2048×2048 with eviction counter (`renderer.rs`).

## Tests and benchmarks

- Rust tests: CSS 39, DOM 22, layout 8, gfx 12, core 34, JS 27, runtime 4 bundle + 10 session = 156 total. All pass (`cargo test --workspace`).
- CLI tests: 9 pass (`node --test packages/cli/test/*.test.mjs`).
- Bench: `cargo run --release -p rendrix-runtime -- bench 1000` (see PERFORMANCE_AUDIT.md).
- E2E: `npm run e2e` 26 scripted steps pass.

## Source references

- Engine pipeline: `engine/crates/rendrix-core/src/lib.rs` (`Engine::update`, `has_pending_changes`, `needs_update`).
- CSS: `engine/crates/rendrix-css/src/{parser,cascade,properties,style,selector,animation}.rs`.
- DOM: `engine/crates/rendrix-dom/src/lib.rs`, `html.rs`, `query.rs`.
- Layout: `engine/crates/rendrix-layout/src/lib.rs`, `taffy_engine.rs`.
- Graphics: `engine/crates/rendrix-gfx/src/{renderer,text,display,context}.rs`, `shader.wgsl`.
- JS: `engine/crates/rendrix-js/src/{quickjs,host,runtime,engine_trait}.rs`, `prelude/*.js`.
- Runtime: `engine/crates/rendrix-runtime/src/{session,window,main,headless,bundle,bench}.rs`.
- CLI build: `packages/cli/src/app/build.mjs`, `commands.mjs`.
- Types: `packages/types/index.d.ts`, `jsx-runtime.d.ts`.
