# Compatibility matrix

Rendrix is **not a browser** and does not claim browser compatibility. This page lists what is
implemented and covered by an automated test (✅), implemented with known gaps (🟡), and not
implemented (❌). Anything not listed is not supported. Tests live next to the code:
`engine/crates/*/src` (unit), `engine/crates/rendrix-runtime/tests` (input/frame integration, GPU pixels),
`packages/cli/test` (build pipeline, runs bundles in the real runtime).

Verified platform: **Windows 11, NVIDIA GeForce RTX 3060, Vulkan** (windowed at 125 % DPI and headless).
macOS and Linux share the same code (winit, wgpu, arboard) but have **not been built or run**.

## JavaScript and modules

| Feature | Status | Notes |
|---|---|---|
| ES2023 language, classes, private fields, `async`/`await`, generators | ✅ | Whatever QuickJS-ng provides; `BigInt`, `WeakRef`, `FinalizationRegistry`, `Object.groupBy`, `toSorted`, `Array.fromAsync` are present |
| ES modules, relative imports, top-level `await` | ✅ | Bare specifiers resolved at build time (ESM packages only) |
| Dynamic `import()` | ✅ | Provided by QuickJS; the build rewrites static and dynamic specifiers to bundle paths (variable specifiers pass through to the runtime loader). Covered by CLI rewrite tests and `rendrix-js` chunk/TLA ordering tests plus a `Session` end-to-end test |
| CommonJS / `require` / Node built-ins | ❌ | Build error with an explanation |
| TypeScript, TSX | ✅ | Same transpile/bundle path as JS; type-checked with project `tsconfig.json` (`allowJs: true`, `checkJs: false` default; `rendrix build/check --check-js` opts JS into checking). Directories with TS/JSX must go through `rendrix dev`/`build` — `AppBundle::from_directory` is dev-JS-only and errors actionably |
| Source maps | ✅ | `tsc` maps stored per module in `bundle.sourceMaps` (`rendrix-app/1` unchanged — additive field); runtime remaps `at name (file:line:col)` frames, invalid maps ignored |
| Promises, microtask order, `queueMicrotask`, unhandled-rejection events | ✅ | Runaway scripts are interrupted by an execution budget (default 5 s/task) and reported as errors |
| `setTimeout`/`setInterval`/`requestAnimationFrame` | ✅ | Frame-driven rAF; timers on the platform clock |
| `Intl`, `WebAssembly`, Workers, `Temporal` | ❌ | Not defined (checked in the runtime); `toLocaleString` works without locale data |

## DOM

| Feature | Status | Notes |
|---|---|---|
| Node, Element, Text, Comment, Document, DocumentFragment, tree mutation, errors as `DOMException` | ✅ | |
| Attributes, `classList`, `dataset`, `style` (live), `innerHTML`/`outerHTML`, `insertAdjacent*`, `cloneNode` | ✅ | HTML parser is tolerant, not the full HTML5 tree builder (no foster parenting) |
| `querySelector(All)`, `matches`, `closest`, `getElementsBy*` | ✅ | Type/class/id/attribute selectors, all combinators, `:not :is :where :nth-* :first/last-child :empty :root :hover :active :focus :disabled :checked`. `:has()` ❌ |
| Events: capture/target/bubble, `once`, `passive`, `signal`, `on*` properties, `stopImmediatePropagation`, `CustomEvent` | ✅ | |
| Mouse/Pointer/Wheel/Keyboard/Input/Focus/Submit events from the OS | ✅ | Touch and gamepad input are ❌; pointer capture is a no-op |
| Form controls: `input` (text-like, checkbox, radio, button), `textarea`, `select`, `button`, `label`, `form` | 🟡 | Typing, caret, selection, clipboard, `input`/`change`, label and submit activation work. No undo, no validation UI, `select` has no popup list, file/date/range inputs ❌ |
| `getBoundingClientRect`, `offset*`, `client*`, `scroll*`, `scrollIntoView`, `elementFromPoint`, `getComputedStyle` | 🟡 | `getComputedStyle` implements a fixed list of properties (display, position, colour, size, margin/padding/border, font, opacity, overflow, z-index, transform, filter, backdrop-filter, clip-path, mask-image, text-shadow, text-overflow, custom properties); others return `""` |
| `MutationObserver` | ✅ | childList, attributes (+filter, old value), characterData, subtree |
| `ResizeObserver` | 🟡 | One content-box size; delivered per frame |
| `IntersectionObserver`, Shadow DOM, custom elements, `Range`/`Selection`, `contenteditable`, drag and drop | ❌ | APIs are absent or throw `NotSupportedError` so feature detection works |

## CSS

| Feature | Status | Notes |
|---|---|---|
| Cascade: origin, specificity, `!important`, source order, inheritance, `inherit`/`initial`/`unset`, custom properties and `var()` with fallbacks | ✅ | |
| Media queries (`width`, `height`, `orientation`, `prefers-color-scheme`, `resolution`, `and`/`not`/`,`) | ✅ | Re-evaluated on resize |
| Box model, `box-sizing`, margin/padding/border, `border-radius` | ✅ | Per-side border colour and dashed/dotted styles render as one solid colour |
| `display`: block, inline, inline-block, flex, inline-flex, grid, table roles, list-item, none, contents | ✅/🟡 | Block/flex/grid/none/contents ✅. Table roles (`table`, `table-row`, `table-cell`, row groups, caption, columns) parse and collapse to a grid-like layout (🟡, see Tables row). `list-item` behaves as block; `<li>` keeps its marker run |
| Flexbox (all properties) | ✅ | Taffy |
| Grid (tracks, `repeat`, `minmax`, `fr`, auto-fill/fit, line and span placement, gaps) | ✅ | `grid-template-areas` ❌ |
| Positioning: relative, absolute, fixed, sticky, z-index and stacking order | 🟡 | `sticky` lays out at its static position and clamps inside the nearest scrollport at paint time (release within the containing block is not modelled); static position of auto-inset absolutes is the container origin |
| Overflow: hidden/clip/scroll/auto, wheel scrolling, nested scrollers | ✅ | Scrollbars are overlay indicators, not draggable |
| Tables (`display: table` / `table-row` / `table-cell`, row groups, `caption`) | 🟡 | Collapses to a grid-like layout: one `1fr` track per column (an explicit `grid-template-*` wins), rows and row groups dissolve, cells are grid items, `col`/`colgroup` generate no box. `colspan`/`rowspan`, automatic column sizing, anonymous table objects and missing-`tbody` insertion are ❌ |
| Floats (`float: left` / `right`, `clear`) | 🟡 | Floated boxes pin to the content edge (explicit widths only; auto-width floats size like blocks) and stay in-flow; `clear` pushes the box (and everything after it) below preceding floats in the same block. No text wrap around floats, no side-by-side packing (floats stack vertically), containers do not auto-expand around floats, floats never interact across containing blocks |
| Multi-column (`column-count`, `column-width`, `columns`, `column-span: all`) | 🟡 | The container becomes N equal `1fr` tracks (from `column-count`, or derived from `column-width` against the viewport width); children flow row-wise. No balancing, no `column-fill`, non-`all` spanners ignored, `column-rule` ❌ |
| `writing-mode` | ❌ | |
| Text: font family/size/weight/style, line-height, letter-spacing, alignment, decoration, transform, `white-space`, wrapping | ✅ | Shaping is cosmic-text (bidi/complex scripts are not covered by tests). `text-overflow: ellipsis` parsed, not drawn; `text-shadow` renders as shadow-tinted text copies beneath the text (offset exact; blur softens through reduced alpha — a box-blur approximation). `word-break`, `line-clamp` ❌ |
| Colours (hex, `rgb`, `hsl`, named, alpha), gradients (linear/radial, ≤ 4 stops), `box-shadow` (outer + inset), `opacity`, `background-image: url()`, `object-fit` | 🟡 | Shadow blur is an analytic approximation; opacity multiplies through one stack (exact for non-overlapping content; overlapping translucent siblings blend pairwise instead of one isolated group — needs an offscreen pass) |
| 2D `transform`, `transform-origin` | ✅ | No 3D, no `perspective` |
| Transitions and `@keyframes` animations | ✅ | Animatable: opacity, colours, lengths, margin/padding, inset, transform, box-shadow, gap, flex-grow, font-size. Others switch discretely |
| `calc()` | 🟡 | Only when all operands are the same kind (px-only or %-only) |
| `@font-face` | 🟡 | Local bundled TTF/OTF/TTC only; WOFF/WOFF2 ❌ |
| `::before`/`::after` with string `content` | ✅ | `attr()`, counters, images ❌ |
| `filter`, `backdrop-filter`, `clip-path`, `mask` | 🟡 | `filter`: `blur()`/`brightness()`/`grayscale()` (brightness/grayscale exact pointwise; `blur()` is a centre + 4-offset copy expansion — a box-blur approximation). `backdrop-filter`: parsed, creates a stacking context, no visual effect yet (sampling the backdrop needs an offscreen pass). `clip-path`: `inset()` exact, `circle()`/`ellipse()` exact, `polygon()` culls to the bounding box (per-pixel polygon clipping needs an offscreen pass). `mask-image: url()` applies the mask's average alpha through the opacity stack (uniform masks exact; spatially-varying masks approximated). All four establish a stacking context. Still ❌: `@container`, `@layer` ordering, nesting, `@import` (`@supports`/`@layer` blocks are flattened) |

## Web platform APIs

| API | Status | Notes |
|---|---|---|
| `URL`, `URLSearchParams` | ✅ | No IDNA/punycode |
| `TextEncoder`, `TextDecoder` | ✅ | UTF-8, latin1/ascii |
| `AbortController`/`AbortSignal` (`timeout`, `any`) | ✅ | |
| `fetch`, `Headers`, `Request`, `Response`, `Blob`, `File`, `FormData` | 🟡 | Relative URLs read bundled assets; `data:` URLs; `http(s)` only when `permissions.network` is true. `FormData` encodes RFC 7578 multipart; a per-app session cookie jar persists cookies for the process lifetime (no expiry enforcement, no `document.cookie` sync — `cookieEnabled` stays `false`). No streaming `ReadableStream` bodies (`Body.body` is `null`; use `text()`/`json()`/`arrayBuffer()`), no CORS/cache — documented native scope: Rendrix is a single-origin `rendrix://app` runtime, not a browser |
| `WebSocket` | ✅ | `ws://`/`wss://` (rustls, native cert roots posture matching `ureq`), subprotocols, text/binary (binaryType `arraybuffer`/`blob`), `bufferedAmount`, close codes/reasons, `wss` needs `permissions.network` (loopback `ws://127.0.0.1` is always allowed for tests). Delivery is frame-budgeted (≤16 messages/frame, spill to next tick) and never blocks the frame loop |
| `localStorage`, `sessionStorage` | ✅ | Local storage is a per-user JSON file keyed by app name; no `storage` events |
| `crypto.getRandomValues`, `randomUUID` | ✅ | OS CSPRNG; `crypto.subtle` ❌ |
| `structuredClone`, `atob`/`btoa`, `performance.now`, `matchMedia`, `console` | ✅ | |
| `navigator.clipboard` | 🟡 | Text only; fed by Copy/Cut in text controls |
| Images via `<img>`, CSS backgrounds, `new Image()` with `load`/`error` | 🟡 | PNG is tested. JPEG, GIF (first frame) and WebP go through the same `image`-crate decoder but have no tests. SVG (`.svg` files, inline `<svg>`, and SVG `drawImage` sources): `rect`/`circle`/`ellipse`/`line`/`polyline`/`polygon`/`path` (M/L/H/V/C/S/Q/T/Z) with solid fills and strokes, CPU-rasterized with no new dependencies (hand-rolled, mirrors the Canvas2D raster conventions); `<text>`, gradients, filters, masks, percentage lengths, external refs ❌. No animation |
| `location`, `history` | 🟡 | In-memory; there is no navigation |
| Canvas 2D | ✅ (subset) | `getContext('2d')` returns a full CPU-rasterized context composited as a GPU texture: paths/fill (`nonzero`/`evenodd`)/stroke, transforms, linear/radial/conic gradients, `Path2D` (own path object for `fill`/`stroke`/`clip`; no SVG-string constructor), `clip()` (intersecting, restored on `save`/`restore`), `fillText`/`strokeText`/`measureText` with `font`/`textAlign`/`textBaseline` (shaped through the shared `TextSystem`; glyphs stay axis-aligned under rotation/skew), shadows (`shadowColor`/`Blur`/`OffsetX`/`OffsetY`, box-blur approximation; `drawImage` casts no shadow), `drawImage`, `get/putImageData`, `toDataURL` (PNG). Still unsupported: `webgpu`/`bitmaprenderer` (null), patterns, filters, non-`source-over` `globalCompositeOperation` (set is ignored, get returns `source-over`), `OffscreenCanvas`, video sources, `toBlob`. Canvas draws mark only the canvas element dirty |
| Canvas 3D (WebGL subset + `RendrixScene`) | 🟡 (subset) | Primary: `RendrixScene.createScene(canvas, desc)` / `setCamera` / `addBox` / `addPlane` / `update` / `remove` (+ `Rendrix3D.createBoxScene` helper); alias: `canvas.getContext('webgl'|'webgl2')` with `clearColor`/`clear`/`enable(DEPTH_TEST)`/`viewport`, `createBuffer`/`bindBuffer`/`bufferData`, `drawArrays(TRIANGLES)` and a `{position,color}` uniform path. Scenes are a JSON desc (`camera{ fovY,near,far,position,lookAt }`, `meshes[{ geo{ kind:"box"|"plane"|"sphere"|"tris",size|verts }, material{ color,metal,rough,emissive }, transform{ pos,rotEulerDeg,scale } }]`, `lights`, `background`) CPU-rasterized into the canvas bitmap (flat Lambert + ambient, painter's algorithm, no depth buffer/backface culling/clipping) and uploaded through the canvas texture path — visible in windowed frames and headless screenshots. A scene-owned canvas repaints fully per upload (2D ops on the same canvas are overwritten); empty transparent scenes never touch the bitmap. Everything else (`webgpu`, textures/framebuffers, `drawElements`, shaders beyond stubs, glTF, shadows, PBR) is ❌ or warn-and-ignore: unsupported calls log `console.warn` and no-op (never crash). `<rendrix-scene>` (and `<canvas data-3d>`) hosts attach/round-trip/remove through this API identically; the GPU scene backend composites those tags from its own store when driven (`rendrix-gfx/src/scene3d.rs`, `Item::Scene3D`), while this shim renders into plain-`<canvas>` bitmaps. Per-frame upload only when dirty; scenes die with their element |
| Dedicated Workers, Service Workers | 🟡/❌ | Dedicated `Worker` ✅: isolated `JsRuntime` per thread, `postMessage`/`onmessage`, binary/`structuredClone` payloads, error events, `terminate()`/`close()`, timers/console (data: `fetch` only), 16-msg/frame pump cap, 500 ms bounded shutdown. No `SharedArrayBuffer`, no DOM/window access, no nested workers, `transfer` is cloned. Service Workers ❌ |
| XMLHttpRequest, IndexedDB, Notifications, Geolocation | ❌ | Not defined |
| `<video>`/`<audio>` | ❌ | Not implemented |
| Drag and drop | 🟡 | Native OS file drops deliver `dragenter`/`dragover`/`drop`/`dragleave` with a `DataTransfer` whose `files` carry `name` + Rendrix-extension `path` (read bytes only via `rendrix.fs` with permission). No draggable-element DnD, no `dragstart` synthesis from script yet |
| `matchMedia` refinement | 🟡 | `prefers-reduced-motion` and `prefers-contrast` are answered from host OS prefs (env override `RENDRIX_REDUCED_MOTION=1` / `RENDRIX_CONTRAST_MORE=1` until per-OS queries land) |
| `rendrix` module: `window`, `clipboard`, `quit`, `fs`, `dialog`, `shell`, `update` | ✅ | `fs` is limited to directories listed in `permissions.fs_read`/`fs_write`; `dialog`/`shell`/`update` documented below |

## Measured performance (M0 harness)

Pipeline numbers live in [PERFORMANCE_AUDIT.md](PERFORMANCE_AUDIT.md) (the `bench 1000`
scenarios plus the M0 per-scenario `shapes`/`restyles`/`layouts` counters). Node-side
build costs, measured on this machine (Windows 11, RTX 3060) with
`node packages/cli/bench/transpile.bench.mjs`:

| Build step | Median |
|---|---:|
| `transpileModule`, one 1 KB TS file | 1.26 ms (200 iters) |
| `buildApp`, 200-module project, no typecheck | 392.07 ms (5 iters) |
| `buildApp`, 200-module project, with typecheck | 237.50 ms (3 iters, warm cache — run after the no-typecheck group) |

Micro-benches (Criterion, dev-only, never shipped): `engine/crates/*/benches/`
(`css`, `engine`, `taffy`, `text`, `js`, `pipeline`). Timing is informational in CI;
allocation/shape-count deltas are asserted in `rendrix-runtime/tests/regression.rs`.

## Conformance (WPT subset)

[WPT.md](WPT.md) tracks a curated CSS layout subset (tables, floats, multi-column, plus
grid/flexbox regression guards) executed against the real headless runtime with
`node scripts/wpt.mjs` (needs a built runtime). Pass rate is appended to
`scripts/wpt-history.json` on every run and rendered into that page. It measures the
subset only — no browser parity is claimed, and it is not the full web-platform-tests suite.

## Application platform

| Capability | Status |
|---|---|
| Native window (winit), DPI scaling, resize, title, close | ✅ Windows (verified); macOS/Linux ❓ untested |
| Mouse, keyboard, wheel, text input, IME commit | ✅ Windows (IME composition itself untested) |
| System clipboard | ✅ via `arboard` (text) |
| Single standalone executable (`rendrix package`) | ✅ Windows (13.8 MB, no external runtime) |
| Single standalone executable (`rendrix package`) | ✅ Windows (13.8 MB, no external runtime); macOS `.app` bundle and Linux binary layouts wired in the CLI (built, not run here) |
| File dialogs, multi-window, tray, global shortcuts, auto-update, accessibility tree | 🟡/❌ | Dialogs, multi-window, the update client and the accessibility bridge are implemented (rows below); tray and global shortcuts remain ❌ |
| Native file dialogs | ✅ | `rendrix.dialog.showOpenDialog({ title, filters: [{ name, extensions }], multiple, properties: ['openDirectory'] })` and `showSaveDialog({ title, defaultPath, filters })` → `Promise<string[] | null>`; OS UI runs on a worker thread (never blocks the frame loop); `Headless::set_dialog_mock` / `host.set_dialog_mock` for tests |
| External links | ✅ | `rendrix.shell.openExternal(url)` — `http(s)`/`mailto:` only, fire-and-forget |
| Multi-window | 🟡 | `window.open(url, target, 'width=W,height=H')` boots a real second window with an isolated `Session` (own engine + JS context); `handle.close()` / per-window close box drops it; last window out quits. No shared document/clipboard beyond the OS clipboard yet |
| Accessibility tree | 🟡 | AccessKit bridge (`accesskit_winit`: UIA/NSAccessibility/AT-SPI) with role/name/state/bounds built from the DOM, AT actions (Click/Focus/Blur/SetValue/ReplaceSelectedText) routed to session entry points, updates pushed on repainted frames only; `prefers-reduced-motion`/`prefers-contrast` via `matchMedia`. No manual Narrator/NVDA/VoiceOver/Orca pass yet — do not claim support (unexecuted procedure: [docs/A11Y_CHECKLIST.md](A11Y_CHECKLIST.md)) |
| App updates | 🟡 | `rendrix.update.check(feedUrl, version?)` → manifest `{ version, url, sha256 }`; `download(url, sha256)` verifies SHA-256 into a staged file; `install(path)` swaps the binary and relaunches (`quitAndInstall` semantics; Windows move-aside rename). Manifest written by `rendrix package --manifest` |
| Code signing / installers | 🟡 | Helpers wired: `rendrix package --sign` (RENDRIX_SIGN_COMMAND / platform env), `--installer nsis` (Windows), `--manifest/--manifest-key/--publish-url`; macOS notarization helper present. No signed/macOS/Linux install runs yet |
| Hot reload (`rendrix dev`) | ✅ | Rebuild diffs `bundle.modules`, writes `.rendrix/hot-update.json`; the running app picks it up via `RENDRIX_HOT_FILE` polling and hot-swaps modules in a fresh JS runtime on the live DOM (state preservation is script-cooperative; failed updates keep the last-good tree with the error logged). `--no-hot` restores restart behavior. End-to-end no-restart path verified at session level + CLI diff/sidecar tests |
| Inspector | 🟡 | `rendrix-runtime --inspect out.json` (headless) dumps a JSON snapshot: DOM node table (capped at 500), computed styles, layout rects, `EngineStats` (incl. shaping signals), console tail, runtime errors. Snapshot on demand only, zero per-frame cost. No overlay UI |
| Headless page render (`rendrix render page.html -o out.png` / `out.pdf` / `frames/`) | ✅ | PNG screenshot, one-page PDF, or a PNG frame sequence (the handoff the /video pipeline consumes; mp4 encoding is explicitly out of scope) through the existing headless path (`--screenshot` / `--pdf` / `--frame-dir`). Typechecking off; only the page plus the files it references are staged |
| Solid adapter | ✅ | `packages/solid-adapter` (upstream `solid-js@1.9.19`, adapter-scoped dep) renders on the Rendrix DOM: mount/update/unmount/cleanup/events/`Show`/`For`/memo, `h` + `jsx-runtime` mapping, `onMount`/`onCleanup`, click handling. Headless 13-step matrix green. Limitations: `SVGElement` shim included; post-dispatch `currentTarget` cleanup warning is benign; `classList`-object-with-accessors is static (use function-valued props); `Show`/`For` need accessor props; no SSR/hydration; TSX `jsxImportSource` pragma unsupported (`buildApp` pins `rendrix`) |

## Web target (wasm32-unknown-unknown, `rendrix-web`)

The same engine (CSS, DOM, layout, text, wgpu renderer, Web API prelude) compiled to WebAssembly.
It runs in a dedicated Worker (`engine/crates/rendrix-web`): the page owns the real `<canvas>` and the
OS events and forwards them (`web/rendrix-web.js`); the worker runs the rAF loop (`web/rendrix-worker.js`)
and reports back what the page must do (cursor, soft keyboard, title, clipboard writes, external links).
`cargo check --target wasm32-unknown-unknown` passes for
`rendrix-css/-dom/-layout/-core/-gfx/-js`, `rendrix-runtime` (lib + the native-only CLI, which is a
wasm stub) and `rendrix-web`. Native-only code in `rendrix-js/src/host.rs` (`std::thread` fetch,
`rfd` dialogs, shell `Command`, the updater, `tungstenite`, worker threads) and the QuickJS backend
(`rquickjs`) are gated `not(wasm32)`; timers use `web_time::Instant` (`std::time::Instant` panics on wasm).

| Capability | Status | Notes |
|---|---|---|
| JavaScript engine | 🟡 | The browser's own engine, not QuickJS: `BrowserEngine` (`rendrix-web/src/engine.rs`) routes `__native(op, ...args)` to the same `DomHost`, runs the same prelude, and forwards console output to the page. No bundling of an engine in the wasm binary; browser JIT performance |
| ES modules | 🟡 | Each bundle module becomes a Blob URL with static specifiers rewritten post-order to dependency URLs, so the browser loader does linking, TLA and live bindings. Literal and computed `import()` go through `__rendrix_dynImport`, resolved against the bundle map at call time; unknown specifiers reject. **Static import cycles cannot be expressed with Blob URLs and are reported as errors** — keep the module graph acyclic for the web target. `boot` (`DOMContentLoaded`/`load`) is deferred until the module queue is idle, as in a page |
| Tasks and microtasks | 🟡 | Microtasks cannot be drained on demand (`run_jobs` is a no-op; promise jobs run when control returns to the worker event loop). The frame loop still settles several extra frames after input so queued jobs land. **No execution-budget interrupt**: a runaway script hangs the worker (the page stays alive), whereas native QuickJS is preempted after 5 s/task |
| Rendering | 🟡 | wgpu `BROWSER_WEBGPU \| GL` on the transferred `OffscreenCanvas`, with downlevel WebGL2 limits requested where WebGPU is missing. Instance creation probes for a *usable* WebGPU adapter (`new_instance_with_webgpu_detection`): a bare `Instance::new` locks onto the WebGPU-only path from `navigator.gpu` alone, which headless Chromium (SwiftShader) exposes without being able to create an adapter. Headless Chromium 153 run is green (10-check driver test: mount, rAF frames, in-app clicks, storage, dialog rejection, WS error event) with SwiftShader WebGPU flags (`--enable-unsafe-swiftshader --in-process-gpu --disable-gpu-sandbox`); real-GPU runs are untested. The renderer itself is not WebGL2-compatible yet — bind-group creation panics there (vertex-stage read-only storage buffers exceed `downlevel_webgl2_defaults`), so a true no-WebGPU browser still fails at startup: WebGL2 renderer support is follow-up work |
| Input and environment | ✅ | Pointer/touch/wheel, keys, IME composition, text commit, paste, resize/DPR, focus, safe area, keyboard height, `prefers-color-scheme`, `maxTouchPoints`, gamepads (polled on the page — the Gamepad API is main-thread only) are forwarded by `rendrix-web.js`. `WebApp::eval` evaluates expressions in the app for diagnostics/tests |
| `fetch` | ✅ | Browser `fetch` behind the host HTTP hook (forbidden headers are dropped by the browser); `data:` and bundle-relative URLs as on native |
| Clipboard | 🟡 | App copies are forwarded to `navigator.clipboard`; pastes arrive as page events. No `clipboardchange` events |
| `localStorage`/`sessionStorage` | 🟡 | In-memory only on the web target; nothing is persisted to browser `localStorage` yet (hook is follow-up work) |
| `rendrix.fs` | ❌ | No filesystem on the web target: every op fails with the permission error |
| `rendrix.dialog` | 🟡 | Rejects actionably (`file dialogs are not available on the web target (use an <input type=file> element instead)`) instead of pretending the user cancelled |
| `rendrix.shell.openExternal` | ✅ | Forwarded to the page, which opens `http(s)`/`mailto:` in a new tab (`noopener,noreferrer`) |
| `WebSocket` | ❌ | The socket handshake needs an OS thread per connection: script gets an `error` event (`WebSocket is not available on the web target yet`). Browser-backed sockets are follow-up work |
| Dedicated Workers | ❌ | Spawning answers `error` + `close` (`Workers are not supported on the web target yet`); the main-thread `Worker` class still exists for feature detection. Browser-Worker backing is follow-up work |
| `rendrix.update` | ❌ | Rejects actionably (`app updates do not apply to the web target`) |
| `matchMedia` refinement | 🟡 | `prefers-color-scheme` follows the page; `prefers-reduced-motion`/`prefers-contrast` still read the (absent on wasm) env overrides — pushing the browser settings through is follow-up work |

## Mobile entry scaffolding (follow-up — no NDK/mac toolchain in this environment)

`Session` is platform-independent: touch (tap/pan/fling/long-press/multi-touch/`touch-action`), safe-area
and keyboard insets as `env()`, soft-keyboard intent and gamepads are driven through its public entry points
and covered by `engine/crates/rendrix-runtime/tests/mobile.rs`, which runs on a desktop with no device or
emulator. The OS shells that would drive `Session` on phones do not exist yet:

- Android: `rendrix-runtime/src/lib.rs` declares a `#[cfg(target_os = "android")] pub mod android;`
  whose file is not present — the NDK shell (activity entry, `Session` driver, APK asset loading,
  memory clipboard, gamepad routing) is unwritten. Nothing has been built for Android.
- iOS: no Swift/ObjC shell (wgpu Metal surface + `Session` driver) exists. Nothing has been built for iOS.
- The desktop-only backends already exclude these targets by cfg (`arboard`/`gilrs` are
  `not(any(android, ios, wasm32))`; `Session` takes any `Clipboard`, e.g. the in-tree memory one), so the
  shells above only need to supply platform plumbing, not engine changes.
