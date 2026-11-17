# Rendrix Dependency Audit

Date of audit: 2026-11-14. Workspace: `engine/Cargo.toml`, `packages/cli/package.json`. No dependencies were added or removed during this audit.

## Classification key

1. **Essential infrastructure worth keeping.**
2. **Replaceable dependency.**
3. **Functionality that should be implemented internally.**
4. **Unnecessary dependency that could be removed.**

## Rust workspace dependencies

| Dependency | Version | Used by | Does | Why used | Performance implications | Class | Recommendation |
|---|---|---|---|---|---|---|---|
| `taffy` | 0.9 (flexbox, grid, block_layout, content_size, taffy_tree) | `rendrix-layout` | Flexbox/grid/block layout engine | Mature, tested layout; incremental tree reuse | Layout is ~5–12 ms; Taffy is not the bottleneck; incremental results reused | 1 Essential | Keep. Replacing would cost months for no gain. |
| `cosmic-text` | 0.14 | `rendrix-gfx` | Text shaping, font DB, swash rasterization | Complex scripts, bidi, font fallback, glyph rasterization | First-frame font scan ~0.75 s; paragraph cache amortizes shaping | 1 Essential | Keep. Do not rewrite text shaping. |
| `wgpu` | 25 | `rendrix-gfx`, `rendrix-runtime` | GPU abstraction (Vulkan/Metal/DX12) | Cross-platform GPU rendering, single draw call | GPU 0.3–0.6 ms/frame; instance buffer upload per frame | 1 Essential | Keep. Core to the renderer. |
| `winit` | 0.30 | `rendrix-runtime` | Windowing, input, event loop, IME | OS window/input integration | Negligible; event-driven | 1 Essential | Keep. |
| `pollster` | 0.4 | `rendrix-gfx`, `rendrix-runtime` | Block on async (GPU init, readback) | Synchronous GPU initialization | Only at startup/readback | 1 Essential | Keep. |
| `bytemuck` | 1 | `rendrix-gfx` | Pod/Zeroable casts for GPU buffers | Safe transmute of instance structs | Zero-cost | 1 Essential | Keep. |
| `rquickjs` | =0.14.0 (loader, futures) | `rendrix-js` | QuickJS bindings (JS engine) | ES2023, ES modules, promises, top-level await | JS execution overhead small; microtask pump per task | 2 Replaceable (but keep) | Keep behind `JsEngine` trait; trait boundary already exists. Replacing with V8 would improve throughput but increase binary size/startup and complexity. Not justified now. |
| `image` | 0.25 (png, jpeg, gif, webp) | `rendrix-gfx`, `rendrix-core`, `rendrix-runtime` | Image decoding | PNG/JPEG/GIF/WebP decode | Decode cost not measured; only PNG tested | 1 Essential (could be 2) | Keep. Could trim features to png-only if size matters, but JPEG/GIF/WebP are declared supported. |
| `serde` / `serde_json` | 1 | `rendrix-js`, `rendrix-runtime`, `rendrix-core` | Serialization (bundles, storage, fetch) | Bundle format, localStorage, asset provider | Minor | 1 Essential | Keep. |
| `getrandom` | 0.2 | `rendrix-js` | OS CSPRNG for crypto.getRandomValues | Backs crypto API | Negligible | 1 Essential | Keep. |
| `ureq` | 2 (tls) | `rendrix-js` | HTTP client for fetch | Lightweight sync HTTP with TLS | Network-bound; thread per request | 2 Replaceable | Keep for now; replace only if HTTP/2/streaming needed (currently not supported). |
| `arboard` | 3 | `rendrix-runtime` | System clipboard (text) | Native clipboard access | Negligible | 1 Essential | Keep. |

## Internal crates (original implementation)

| Crate | Classification | Notes |
|---|---|---|
| `rendrix-css` | 3 Implemented internally | Original CSS engine (parser, selectors, cascade, computed style). Keep internal; this is the differentiator. |
| `rendrix-dom` | 3 Implemented internally | Original arena DOM. Keep internal. |
| `rendrix-core` | 3 Implemented internally | Original engine pipeline. Keep internal. |
| `rendrix-js` (host/prelude) | 3 Implemented internally | Web API surface is JavaScript prelude over `__native`; original. Keep. |
| `rendrix-layout` | 1/3 Hybrid | Neutral box tree + trait is original; Taffy is the backend. Keep. |
| `rendrix-gfx` | 1/3 Hybrid | Display list + atlas + renderer original; wgpu/cosmic-text are backends. Keep. |

## JavaScript/npm dependencies (CLI)

| Dependency | Version | Used by | Class | Notes |
|---|---|---|---|---|
| `typescript` | ^5.9.3 (dev) | `packages/cli` build | 1 Essential | Type-checking + transpilation. Required for TS/TSX. Keep. |
| Node.js built-ins (fs, path, etc.) | runtime | CLI | 1 Essential | Build tooling only; not shipped to apps. |

## Dependency-by-dependency recommendations

### QuickJS (`rquickjs`)
- **What it does:** Embedded JavaScript engine providing ES2023, ES modules, promises, top-level await, GC.
- **Why used:** Single binary, no external runtime, fast startup, embeddable; behind a `JsEngine` trait so it can be swapped.
- **Performance:** JS execution is not the measured bottleneck; microtask pump runs per task. Memory limit 1 GB, stack 2 MB.
- **Replace?** Only if profiling shows JS execution as the bottleneck. V8 would improve throughput but cost binary size and startup. Keep QuickJS; the trait boundary already allows replacement.

### wgpu
- **What it does:** Cross-platform GPU API abstraction (Vulkan/Metal/DX12).
- **Why used:** Owns the rendering pipeline; no Chromium.
- **Performance:** GPU submission 0.3–0.6 ms; not a bottleneck.
- **Replace?** No. Mature infrastructure.

### Taffy
- **What it does:** Flexbox/grid/block layout.
- **Why used:** Correct, tested, incremental tree reuse.
- **Performance:** Layout 5–12 ms; Taffy reuses its tree; box generation is the cost, not Taffy.
- **Replace?** No. Implementing layout internally would be a large regression risk with no measurable gain.

### winit
- **What it does:** Window creation, input events, event loop, IME.
- **Why used:** Cross-platform windowing.
- **Performance:** Event-driven; negligible.
- **Replace?** No.

### Text shaping (cosmic-text)
- **What it does:** Font database, shaping, swash rasterization, paragraph cache.
- **Why used:** Complex scripts, font fallback, glyph rasterization.
- **Performance:** First-frame cost ~0.75 s; paragraph cache amortizes.
- **Replace?** No. Could cache the font DB to disk; that is an implementation improvement, not a replacement.

### Image decoding (image crate)
- **What it does:** PNG/JPEG/GIF/WebP decode.
- **Why used:** `<img>`, CSS backgrounds, `new Image()`.
- **Performance:** Not measured; PNG only tested.
- **Replace?** No. Could trim to PNG-only if binary size becomes a priority, but would break declared JPEG/GIF/WebP support.

### CSS parsing (rendrix-css, internal)
- **What it does:** Original CSS engine.
- **Performance:** 22.7 ms full restyle on resize at the time of the audit; stylesheet caching and
  viewport-dependency pruning later reduced this to ~0.35 ms of CSS work in the last clean `bench 1000` run.
- **Replace?** N/A (internal). Keep improving invalidation; the at-rule work (`@supports`, `@layer`,
  `@container`) is implemented internally.

### DOM (rendrix-dom, internal)
- **What it does:** Original arena DOM.
- **Performance:** Arena allocation; GC of detached subtrees.
- **Replace?** N/A.

## Unnecessary / removable candidates

- `rendrix-runtime` depends on `image` for screenshot saving (`image::save_buffer`); could gate behind a feature if binary size matters.
- Legacy Electron-compat code is already removed (checkpoint tag).

## Additions approved and landed (implementation mission, 2026-11-14)

| Dependency | Version | Used by | Why | Class |
|---|---|---|---|---|
| `rfd` | 0.18 | `rendrix-runtime` | Native file open/save dialogs (A2); tiny sync dialogs, no hand-rolled per-OS code | 1 Essential |
| `tungstenite` | 0.30 | `rendrix-js` (+ dev-dep for echo tests) | WebSocket client framing (A5); `rustls-tls-webpki-roots` matches `ureq` TLS posture | 1 Essential |
| `rustls` | 0.23 | `rendrix-js` | TLS for WebSocket client | 1 Essential |
| `accesskit` | 0.25 | `rendrix-js` (host a11y tree) | Accessibility tree roles/states (A7) | 1 Essential |
| `accesskit_winit` | 0.34 | `rendrix-runtime` | OS-AT bridge (UIA/NSAccessibility/AT-SPI) (A7) | 1 Essential |
| `criterion` | dev-only | all crates (`benches/`) | Micro-benchmarks (M0); never shipped | 1 Essential (dev) |
| `solid-js` | 1.9.19 (exact, adapter-scoped) | `packages/solid-adapter` only | Real fine-grained reactivity for the D4 Solid adapter; no second renderer; never touches the engine | Approved, adapter-scoped |
| `sha2` | 0.11 | `rendrix-js` | `crypto.subtle.digest` (SHA-1/256/384/512) | 1 Essential |
| `gilrs` | 0.11 | `rendrix-runtime` (non-mobile) | Desktop gamepad input forwarded into `Session` | 1 Essential |
| `web-time` | 1 | `rendrix-core`, `-js`, `-runtime` | A `Instant` that works on native and wasm32 (`std::time::Instant` panics on wasm) | 1 Essential |
| `sys-locale`, `glam` | 0.3, 0.34 | workspace | OS locale detection and small vector/matrix math for 3D scene projection | 1 Essential |
| `wasm-bindgen`, `wasm-bindgen-futures`, `js-sys`, `web-sys` | 0.2 / 0.4 / 0.3 / 0.3 | `rendrix-web` only (wasm32) | Bind the same engine to the browser Worker, `OffscreenCanvas` and browser APIs | 1 Essential (web target) |

`packages/cli/src/installer/*` and `signing.mjs` are now wired into `rendrix package --sign --installer nsis --manifest/--manifest-key/--publish-url/--app-version` plus macOS `.app` layout (S1); the "unwired" note above is superseded. The renderer's offscreen compositing (real blur, backdrop-filter, per-pixel polygon clips), Canvas 2D, SVG painting and 3D scene rasterization are implemented internally in `rendrix-gfx` with no new rendering dependencies.

## Summary

- Keep all mature infrastructure (wgpu, Taffy, winit, cosmic-text, QuickJS, image, ureq, arboard).
- Do not rewrite for originality where mature crates exist.
- Original value is in the internal crates: CSS, DOM, core pipeline, JS host/prelude, layout trait, renderer. These should remain internal and be improved, not replaced.
- The `JsEngine` trait and `LayoutEngine` trait already provide replaceability where it matters.
