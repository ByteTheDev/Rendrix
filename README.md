# Rendrix

**If you can build it with TypeScript, you should be able to build it with Rendrix.**

Rendrix is a rendering engine and application runtime. Your TypeScript runs in an embedded JavaScript
engine (QuickJS), builds a real DOM, is styled by a real CSS engine, laid out with flexbox/grid/block
layout and drawn directly on the GPU with wgpu. **No Chromium. No Electron. No WebView.** Rendrix owns the
runtime, the rendering pipeline and the application platform, and is not trying to be Electron or a browser.

> **Status: early.** The first end-to-end milestone works on Windows; the web-platform surface is broad but
> incomplete, and several capabilities are approximations rather than full browser implementations.
> [docs/COMPATIBILITY.md](docs/COMPATIBILITY.md) lists exactly what is supported, partial and missing,
> and only claims behaviour that has a test. macOS and Linux share the code but have not been built or run
> by hand. Rendrix is not a sandbox for hostile code; see [docs/SAFETY.md](docs/SAFETY.md).

```tsx
import { render } from "rendrix/jsx-runtime";

let clicks = 0;
const label = (<p>No clicks yet.</p>) as HTMLParagraphElement;

render(
  <main>
    <h1>Hello, Rendrix</h1>
    {label}
    <button onClick={() => { label.textContent = `Clicked ${++clicks} times.`; }}>Click me</button>
  </main>,
  document.getElementById("root")!,
);
```

```text
TypeScript + CSS ──rendrix build──▶ ES modules ──▶ QuickJS ──▶ DOM ──▶ style ──▶ layout ──▶ display list ──▶ wgpu
                                                       ▲                                                       │
                                   click / key / wheel ┴──────── winit window, DOM events, default actions ◀────┘
```

## Try it

```sh
npm install
npm run runtime                       # cargo build --release -p rendrix-runtime  (needs Rust)
node packages/cli/bin/rendrix.mjs doctor
npm run example                       # examples/app-counter in a native window, rebuilding on change
npm run e2e                           # scripted headless run with screenshots in .rendrix-shots/
npm test                              # Rust workspace + CLI tests
npm run wpt                           # curated CSS layout conformance subset
```

Create your own: `rendrix init my-app && cd my-app && rendrix dev`. Ship it: `rendrix package` produces one
standalone executable (≈13.8 MB on Windows) that needs no Node, Bun or browser. Headless rendering to PNG,
PDF or a PNG frame sequence: `rendrix render page.html -o out.png`.

CLI: `init`, `dev`, `build`, `run`, `check`, `package`, `render`, `doctor`. Details in
[docs/BUILDING.md](docs/BUILDING.md).

## What is in the box

* **Runtime.** QuickJS behind a replaceable `JsEngine` trait, ES modules with top-level `await`, promises,
  timers, `requestAnimationFrame`, TypeScript and TSX compiled at build time, source maps and a 5 s/task
  execution-budget interrupt for runaway scripts. Dedicated Workers run in their own threads.
* **DOM and events.** Node/Element/Document APIs, selectors (including `:has()` and `:focus-visible`),
  forms with undo, constraint validation and pickers, text editing, capture/bubble events driven by real OS
  input, `MutationObserver`, `ResizeObserver`, `IntersectionObserver`, Shadow DOM, custom elements,
  `Range`/`Selection`, `contenteditable` and native file drag-and-drop.
* **CSS.** Cascade, specificity, inheritance, `var()`, media queries, `@supports`, `@layer`, `@container`,
  nesting, `@import`, transitions, keyframes, transforms, gradients, shadows, flexbox, grid, positioning,
  overflow/scrolling, filters, `backdrop-filter`, `clip-path` and text effects. Tables, floats and
  multi-column are grid-like approximations; see the matrix for exact gaps.
* **Rendering.** SDF rounded-rect/border/shadow shader, persistent glyph atlas, display list with culling,
  clipping and 2D transforms, a shared atlas that batches small images into one draw, offscreen compositing
  layers (isolated group opacity, separable Gaussian blur, real backdrop-filter), high-DPI, Canvas 2D
  (CPU-rasterized, GPU texture), SVG painting, and an experimental 3D scene API (`RendrixScene` plus a small
  WebGL subset). A 9,000-node
  page re-renders a hover in ~1 ms and a DOM mutation in ~6.5 ms; resize is ~10 ms
  ([measurements](docs/PERFORMANCE_AUDIT.md)).
* **Web APIs.** `fetch` (with a cookie jar and `FormData`), `WebSocket`, `URL`, `localStorage`,
  `AbortController`, `TextEncoder`/`Decoder`, `matchMedia`, `structuredClone`, `crypto.getRandomValues`
  and `crypto.subtle.digest`, images, clipboard and native dialogs.
* **Application platform.** winit window, DPI, multi-window, OS file dialogs, IME, clipboard,
  drag-and-drop, an AccessKit accessibility bridge, update checks, signing helpers, headless scripted
  testing (`--headless --script`, `--inspect`, `--screenshot`, `--pdf`, `--frame-dir`) and a frame benchmark.
* **Developer ecosystem.** `init/dev/build/run/check/package/render/doctor`, state-preserving hot reload,
  a direct-DOM JSX runtime, a Solid adapter and an experimental WebAssembly target (`rendrix-web`) that runs
  the same engine in a browser Worker with the browser's own JS engine.

Not yet: full browser parity, a manually verified screen-reader pass, macOS/Linux hardware validation,
a browser-verified WebGL2 run (the renderer has a WebGL2 path, tested so far only under native downlevel
limits), browser-backed WebSocket/Workers/storage on the web target,
Service Workers, XHR, IndexedDB, video/audio playback, canvas WebGPU, a React adapter, full table/float
fidelity, `writing-mode`, WOFF2 and complex masks. Rendrix is not a browser and makes no browser-compatibility
claim.

## Repository

| Path | |
|---|---|
| `engine/` | Rust workspace: `rendrix-css`, `-dom`, `-layout`, `-gfx`, `-core`, `-js`, `-runtime`, `-web` |
| `packages/cli/` | The `rendrix` command (Node): build pipeline, packaging, templates, tests |
| `packages/types/` | TypeScript declarations and JSX types (`rendrix`, `rendrix/jsx-runtime`) |
| `packages/solid-adapter/` | Solid reactivity adapter that renders on the Rendrix DOM |
| `examples/app-counter/` | The milestone application and its scripted end-to-end test |
| `examples/app-3d/` | 3D scene example |
| `website/` | Marketing site and in-page documentation tabs (synced from `docs/`) |
| `video/` | Optional Remotion project that renders launch videos from the Render/frame-sequence pipeline (not part of the engine) |
| `docs/` | [Architecture](docs/ARCHITECTURE.md), [compatibility](docs/COMPATIBILITY.md), [building](docs/BUILDING.md), [safety](docs/SAFETY.md), [performance](docs/PERFORMANCE_AUDIT.md), [web performance](docs/WEB_PERFORMANCE.md), [roadmap](docs/ROADMAP.md), [accessibility plan](docs/ACCESSIBILITY_PLAN.md) |

Earlier versions of this project compiled a static HTML subset to a native view tree and shipped an Electron
compatibility layer. That direction was replaced; the old code remains in git history
(tag `checkpoint/pre-engine-transformation`).

MIT licensed.
