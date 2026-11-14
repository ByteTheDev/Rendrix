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
| `getBoundingClientRect`, `offset*`, `client*`, `scroll*`, `scrollIntoView`, `elementFromPoint`, `getComputedStyle` | 🟡 | `getComputedStyle` implements a fixed list of properties (display, position, colour, size, margin/padding/border, font, opacity, overflow, z-index, transform, custom properties); others return `""` |
| `MutationObserver` | ✅ | childList, attributes (+filter, old value), characterData, subtree |
| `ResizeObserver` | 🟡 | One content-box size; delivered per frame |
| `IntersectionObserver`, Shadow DOM, custom elements, `Range`/`Selection`, `contenteditable`, drag and drop | ❌ | APIs are absent or throw `NotSupportedError` so feature detection works |

## CSS

| Feature | Status | Notes |
|---|---|---|
| Cascade: origin, specificity, `!important`, source order, inheritance, `inherit`/`initial`/`unset`, custom properties and `var()` with fallbacks | ✅ | |
| Media queries (`width`, `height`, `orientation`, `prefers-color-scheme`, `resolution`, `and`/`not`/`,`) | ✅ | Re-evaluated on resize |
| Box model, `box-sizing`, margin/padding/border, `border-radius` | ✅ | Per-side border colour and dashed/dotted styles render as one solid colour |
| `display`: block, inline, inline-block, flex, inline-flex, grid, none, contents | ✅ | `table*` and `list-item` are not implemented (lists get a marker run) |
| Flexbox (all properties) | ✅ | Taffy |
| Grid (tracks, `repeat`, `minmax`, `fr`, auto-fill/fit, line and span placement, gaps) | ✅ | `grid-template-areas` ❌ |
| Positioning: relative, absolute, fixed, z-index and stacking order | 🟡 | `sticky` behaves as relative; static position of auto-inset absolutes is the container origin |
| Overflow: hidden/clip/scroll/auto, wheel scrolling, nested scrollers | ✅ | Scrollbars are overlay indicators, not draggable |
| Floats, multi-column, tables, `writing-mode` | ❌ | |
| Text: font family/size/weight/style, line-height, letter-spacing, alignment, decoration, transform, `white-space`, wrapping | ✅ | Shaping is cosmic-text (bidi/complex scripts are not covered by tests). `text-overflow: ellipsis` parsed, not drawn; `text-shadow`, `word-break`, `line-clamp` ❌ |
| Colours (hex, `rgb`, `hsl`, named, alpha), gradients (linear/radial, ≤ 4 stops), `box-shadow` (outer + inset), `opacity`, `background-image: url()`, `object-fit` | 🟡 | Shadow blur is an analytic approximation; opacity applies per item, not as an offscreen group |
| 2D `transform`, `transform-origin` | ✅ | No 3D, no `perspective` |
| Transitions and `@keyframes` animations | ✅ | Animatable: opacity, colours, lengths, margin/padding, inset, transform, box-shadow, gap, flex-grow, font-size. Others switch discretely |
| `calc()` | 🟡 | Only when all operands are the same kind (px-only or %-only) |
| `@font-face` | 🟡 | Local bundled TTF/OTF/TTC only; WOFF/WOFF2 ❌ |
| `::before`/`::after` with string `content` | ✅ | `attr()`, counters, images ❌ |
| `filter`, `backdrop-filter`, `clip-path`, `mask`, `@container`, `@layer` ordering, nesting, `@import` | ❌ | `@supports`/`@layer` blocks are flattened |

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
| Images via `<img>`, CSS backgrounds, `new Image()` with `load`/`error` | 🟡 | PNG is tested. JPEG, GIF (first frame) and WebP go through the same `image`-crate decoder but have no tests. No SVG, no animation |
| `location`, `history` | 🟡 | In-memory; there is no navigation |
| Canvas 2D | ✅ (subset) | `getContext('2d')` returns a full CPU-rasterized context composited as a GPU texture: paths/fill/stroke, transforms, 2-stop linear gradients, `drawImage`, `get/putImageData`, `toDataURL` (PNG). Unsupported: `webgl`/`webgl2`/`webgpu`/`bitmaprenderer` (null), `fillText`/text metrics, radial/conic gradients, patterns, `Path2D`, `clip()`, shadows, filters, `globalCompositeOperation` (always source-over), `OffscreenCanvas`, video/SVG sources, `toBlob`. Canvas draws mark only the canvas element dirty |
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
| Accessibility tree | 🟡 | AccessKit bridge (`accesskit_winit`: UIA/NSAccessibility/AT-SPI) with role/name/state/bounds built from the DOM, AT actions (Click/Focus/Blur/SetValue/ReplaceSelectedText) routed to session entry points, updates pushed on repainted frames only; `prefers-reduced-motion`/`prefers-contrast` via `matchMedia`. No manual Narrator/NVDA/VoiceOver/Orca pass yet — do not claim support |
| App updates | 🟡 | `rendrix.update.check(feedUrl, version?)` → manifest `{ version, url, sha256 }`; `download(url, sha256)` verifies SHA-256 into a staged file; `install(path)` swaps the binary and relaunches (`quitAndInstall` semantics; Windows move-aside rename). Manifest written by `rendrix package --manifest` |
| Code signing / installers | 🟡 | Helpers wired: `rendrix package --sign` (RENDRIX_SIGN_COMMAND / platform env), `--installer nsis` (Windows), `--manifest/--manifest-key/--publish-url`; macOS notarization helper present. No signed/macOS/Linux install runs yet |
| Hot reload (`rendrix dev`) | ✅ | Rebuild diffs `bundle.modules`, writes `.rendrix/hot-update.json`; the running app picks it up via `RENDRIX_HOT_FILE` polling and hot-swaps modules in a fresh JS runtime on the live DOM (state preservation is script-cooperative; failed updates keep the last-good tree with the error logged). `--no-hot` restores restart behavior. End-to-end no-restart path verified at session level + CLI diff/sidecar tests |
| Inspector | 🟡 | `rendrix-runtime --inspect out.json` (headless) dumps a JSON snapshot: DOM node table (capped at 500), computed styles, layout rects, `EngineStats` (incl. shaping signals), console tail, runtime errors. Snapshot on demand only, zero per-frame cost. No overlay UI |
| Solid adapter | ✅ | `packages/solid-adapter` (upstream `solid-js@1.9.19`, adapter-scoped dep) renders on the Rendrix DOM: mount/update/unmount/cleanup/events/`Show`/`For`/memo, `h` + `jsx-runtime` mapping, `onMount`/`onCleanup`, click handling. Headless 13-step matrix green. Limitations: `SVGElement` shim included; post-dispatch `currentTarget` cleanup warning is benign; `classList`-object-with-accessors is static (use function-valued props); `Show`/`For` need accessor props; no SSR/hydration; TSX `jsxImportSource` pragma unsupported (`buildApp` pins `rendrix`) |
