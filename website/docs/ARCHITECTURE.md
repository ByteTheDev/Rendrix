# Architecture

Rendrix is a rendering engine and application runtime. TypeScript applications run inside it and are
drawn by its own GPU pipeline: there is no Chromium, Electron, WebView or system browser involved,
and Rendrix makes no attempt to be API-compatible with any of them.

```
 your TypeScript / TSX            rendrix CLI (Node)                       rendrix-runtime (Rust)
 ───────────────────  build ───▶  type-check, transpile to ES modules ──▶  ┌──────────────────────────────┐
 index.html, styles.css           pack  .rdxapp  (JSON bundle)             │ rendrix-js   QuickJS + Web APIs│
                                  or  standalone executable                │ rendrix-core DOM → style →     │
                                                                           │              boxes → layout →  │
        OS input ◀──────────────────────────────────────────────────────── │              display list      │
        (winit)  ──▶ Session ──▶ DOM events ──▶ your handlers ──▶ DOM ───▶ │ rendrix-gfx  wgpu renderer    │
                                                                           └──────────────────────────────┘
```

## Crates (`engine/crates/`)

| Crate | Responsibility | Interface other crates rely on |
|---|---|---|
| `rendrix-css` | Tokenizer/parser, selectors + specificity, cascade, inheritance, custom properties (`var()`), media queries, `@keyframes`, timing functions, interpolation, computed style | `SelectorElement` trait (style any tree), `ComputedStyle`, `compute_style` |
| `rendrix-dom` | Arena DOM: nodes, attributes, mutation records, selector queries, tolerant HTML parser/serializer, form state, dirty tracking, node GC | `Document`; `ElRef` implements `SelectorElement` |
| `rendrix-layout` | Layout-engine-neutral box tree and the `LayoutEngine` trait; a persistent Taffy backend (flex, grid, block) that only re-lays out what changed | `LayoutNode`, `LayoutEngine`, `TextMeasurer` |
| `rendrix-gfx` | Display list, cosmic-text shaping + paragraph cache + disk font-DB cache, glyph atlas with LRU, wgpu renderer (SDF rounded rects/per-side/dashed borders/gradients/shadows/clips/transforms, images, text), a shared atlas that batches small images into one draw, offscreen compositing layers (isolated opacity, separable Gaussian blur, real backdrop-filter), CPU-rasterized Canvas 2D, SVG painting and 3D scene rasterization with dynamic uniform offsets | `DisplayList`, `TextSystem`, `Renderer`, `Canvas` |
| `rendrix-core` | `Engine`: DOM → styles (+transitions/animations) → incremental box generation and dirty-subtree patching (block/inline/flex/grid, anonymous boxes, whitespace, replaced elements, form controls) → layout → painting; hit testing, scrolling, text editing | `Engine::update(now)`, `hit_test`, `edit`, `display_list` |
| `rendrix-js` | `JsEngine` trait + QuickJS backend, ES module loader, DOM and Web API prelude (JavaScript), host operations, event-loop glue, dedicated workers, source-map remapping and the AccessKit tree builder | `JsEngine`, `Host`, `JsRuntime` |
| `rendrix-runtime` | App bundle loader, `Session` (input state machine and frame loop), winit window driver (multi-window, touch, gamepads, IME), headless driver, benchmark, `--inspect` snapshot | `Session`, `AppBundle` |
| `rendrix-web` | Experimental wasm32 target: the same engine running in a Web Worker with the browser's JS engine and a transferred `OffscreenCanvas` | `WebApp`, `BrowserEngine` |

## One frame

1. **Platform events** (winit) are translated by the window driver into `Session` calls: `pointer_move`,
   `pointer_down`, `key_down`, `wheel`, `text_commit`, `resize`.
2. `Session` hit-tests against the previous frame's geometry and dispatches **real DOM events**
   (`pointerover/enter/move/down/up`, `mouse*`, `click`, `keydown`, `input`, `change`, `focus*`, `wheel`…)
   through the JavaScript event system with capture → target → bubble phases. Default actions run only if
   no handler called `preventDefault()`: focus, click activation (checkbox, radio, label, form submit),
   caret placement and drag-selection, Tab traversal, text editing, scrolling.
3. Timers, completed fetches and microtasks run; then `requestAnimationFrame` callbacks.
4. `Engine::update(now)`:
   - rebuild stylesheets if `<style>`/`<link>` or the media context changed;
   - **restyle** only what changed: dirty subtrees for attribute/class/inline-style changes; for
     hover/active/focus and child-list changes only the element itself, descending when the computed style
     changed or the stylesheet has non-local state/structural selectors;
   - if no computed style actually changed (most hovers), the frame stops here;
   - start transitions/keyframe animations from style changes and sample running ones;
   - patch the **box tree** per element for dirty subtrees instead of rebuilding it (formatting-context
     rules, anonymous boxes) and run layout (Taffy keeps its tree between frames, so unchanged subtrees
     reuse cached results);
   - paint to a **display list** with stacking order, clips, transforms and viewport culling, producing
     the hit-test list in the same traversal. Pixel-reading effects (`filter: blur()`, `backdrop-filter`,
     polygon clips) route through isolated offscreen compositing layers; ordinary frames keep the
     single-draw-call path.
5. `Renderer` flattens the display list into one instance buffer and draws it in a single pipeline and
   usually one draw call. Canvas content is uploaded as its own texture and composited in the same pass.

An idle application does no work: no timer, no animation and no pending change means no frame.

## Replaceable parts

* **JavaScript engine.** Only `quickjs.rs` names `rquickjs`. The rest of the runtime talks to a `JsEngine`
  trait (eval script/module, call a global, drain jobs) and exposes native functionality through a single
  entry point, `__native(op, ...args)`, implemented by `Host`. The Web API surface is JavaScript
  (`prelude/*.js`) built on those operations, so another engine needs the trait implementation and the same
  entry point.
* **Layout engine.** Layout consumes `LayoutNode` + `ComputedStyle`, never DOM types. Taffy is one
  implementation of `LayoutEngine`.
* **Text.** Layout measures through `TextMeasurer`; only `rendrix-gfx` shapes text.

## Modules and TypeScript

The runtime never sees TypeScript. `rendrix build` type-checks with the project's `tsconfig.json`, transpiles
every module to ES2022 with the TypeScript compiler API, resolves bare specifiers from `node_modules`
(ESM only), rewrites imports to bundle paths and emits a `rendrix-app/1` JSON bundle. QuickJS loads the
modules natively (static imports, `import()`, top-level `await`). JSX compiles to
`rendrix/jsx-runtime`, which builds DOM nodes directly (no virtual DOM).

## Memory model

DOM nodes live in a Rust arena. JavaScript wrappers are weak handles registered with a
`FinalizationRegistry`; wrappers of connected nodes are also held strongly so identity, expando properties
and listeners survive GC. Listener state is keyed by node id, never stored on the wrapper. Detached subtrees
with no live wrapper are freed by a periodic sweep and their ids reused.

## Known architectural limits

* Box generation is incremental per dirty subtree, but Taffy still re-solves the affected layout tree
  (≈5–7 ms for a mutation on 9,000 nodes). Getting mutations under 3 ms is the open performance step.
* Overlay scrollbars are painted but not draggable. The AccessKit accessibility tree and OS bridge exist
  ([ACCESSIBILITY_PLAN.md](ACCESSIBILITY_PLAN.md)), but no manual screen-reader pass has been recorded.
* Absolutely positioned boxes use the containing block's origin when their insets are `auto` (no static
  position), and inline elements with box styling (padding, background…) behave as inline-blocks.
* Each window has its own `Session` and JavaScript context; windows do not share a document or clipboard
  beyond the OS clipboard. There is one process and no isolation for hostile code.
* Tables, floats and multi-column are laid out as grid-like approximations; CSS masks, 3D and the web
  target are partial. See [COMPATIBILITY.md](COMPATIBILITY.md).

See [COMPATIBILITY.md](COMPATIBILITY.md) for exactly what is and is not supported, and
[ROADMAP.md](ROADMAP.md) for current status. [ENGINE_ASSESSMENT.md](ENGINE_ASSESSMENT.md) is the dated
assessment from the start of the engine rewrite.
