# Rendrix Implementation Roadmap

Date of original audit: 2026-11-14. **Updated 2026-11-16** to reflect what is actually in the tree.
Status reflects verified source and tests, not README claims. The authoritative feature list is
[COMPATIBILITY.md](COMPATIBILITY.md); measurements are in [PERFORMANCE_AUDIT.md](PERFORMANCE_AUDIT.md).

## Current state

`cargo test --workspace`: **375 passed / 0 failed / 2 ignored** (the two ignored session-level variants
are superseded by engine/renderer-level tests). CLI 22/22, Solid adapter 2/2. A large batch of new work
(CSS at-rules, compositing, DOM APIs, canvas, forms, 3D, web target) is **in the working tree but not yet
committed**; treat "landed" below as "present in source with tests", not "released".

Landed task IDs: R0.2, R1.1–R1.4, R2.1, R2.3 (except tray/shortcuts), R3.1, R3.2, R3.5, R4.1–R4.3,
R5.1 (Solid), R5.2 (snapshot inspector), plus 3D scenes, the wasm web target and mobile-input scaffolding.
Partial: R3.3, R3.4, R2.3 (tray/shortcuts), R6.x distribution. Open: R2.2, the web-target follow-ups,
manual accessibility verification and scale/cold-start measurement.

## Guiding priorities (updated 2026-11-16)

1. Re-measure cold start now that the font database is cached and the atlas uses LRU; target first frame < 0.2 s.
2. Mutation latency under 3 ms (120 FPS): residual Taffy re-solve + `InlineContent::key` rehashing.
3. macOS/Linux validation on real hardware plus the manual screen-reader pass in [A11Y_CHECKLIST.md](A11Y_CHECKLIST.md).
4. Web-target gaps: WebGL2 fallback, browser-backed WebSocket/Workers/storage, import cycles, runaway-script interrupt.
5. Scale measurement: 50k+ nodes and a second machine/GPU.
6. Compatibility gaps with high web value: `cqw`/`cqh`, WOFF2, `writing-mode`, spatially-varying masks,
   table `colspan`/`rowspan`, float text wrap, range input, validation bubble UI.
7. Ecosystem: React reconciler and an overlay inspector on top of the existing `--inspect` snapshot.

## Phase 0 — Stabilize and measure

### R0.1 Uncommitted fix review and commit
- **Status:** in progress. The CSS parser (non-finite keyframe offsets), border-width, QuickJS array unwrap
  and session element-unwrap fixes are present in the working tree alongside the larger feature batch.
- **Missing:** review, add any missing tests and **commit** — nothing here is committed yet.

### R0.2 Expand benchmarks
- **Status:** done. Criterion micro-benchmarks live in `engine/crates/*/benches/` (css, engine, taffy,
  text, js, pipeline) plus a Node transpile bench; allocation/shape assertions in
  `rendrix-runtime/tests/regression.rs`. Timing is informational; budgets are asserted.

## Phase 1 — Performance foundations

### R1.1 Incremental box generation — done
Persistent per-element boxes and dirty-subtree patching replace the full `build_root` rebuild. Correctness
is guarded by `patch_tree_splices_non_root_units_instead_of_rebuilding` and
`patched_tree_matches_full_rebuild_across_mutations`. Mutations improved from ~8.7 ms to ~6.5–7 ms.

### R1.2 Media-query-aware restyle — done
Stylesheet/`RuleSet` caching plus media/viewport-dependency flags skip restyle when viewport-dependent rules
are absent. Viewport resize improved from ~36 ms to ~10 ms (below the 15 ms goal).

### R1.3 Cold-start font cache — landed, needs re-measurement
The font database is serialized to a disk cache (keyed to reject tampering) and shaping uses an LRU with
quantised widths. The old ~0.75 s first-frame figure predates this work; re-run `bench 1000` and replace the
startup numbers in the performance audit before making any claim.

### R1.4 Glyph atlas LRU — done
Least-recently-used eviction replaces the full clear; overflow recovery is covered by a renderer pixel test.

## Phase 2 — Accessibility and platform

### R2.1 Accessibility tree (AccessKit) — code done, verification open
`rendrix-js` builds role/name/state/bounds from the DOM; `accesskit_winit` bridges UIA/NSAccessibility/AT-SPI
and routes AT actions into `Session`. `tests/platform.rs` and `tests/regression.rs` assert tree shape.
**No manual Narrator/NVDA/VoiceOver/Orca pass has been recorded** — see [A11Y_CHECKLIST.md](A11Y_CHECKLIST.md).

### R2.2 macOS and Linux validation — open
CI config builds and tests three desktop targets, but no results are recorded and no hardware run has been
done. Font paths, clipboard and IME are the expected portability risks. Procedure:
[RELEASE_CHECKLIST.md](RELEASE_CHECKLIST.md).

### R2.3 Platform services — mostly done
Native file dialogs (`rfd`), multi-window, native file drag-and-drop, external links and the update client
are implemented and tested (`tests/platform.rs`). **Tray and global shortcuts remain unimplemented.**
Mobile OS shells (Android NDK activity, iOS Metal shell) are not written; the platform-independent `Session`
already handles touch, safe-area/keyboard insets, gamepads and soft-keyboard intent
(`engine/crates/rendrix-runtime/tests/mobile.rs`).

## Phase 3 — Rendering and web platform

### R3.1 Canvas 2D — done (subset)
CPU-rasterized, GPU-composited context: paths, fill/stroke/gradients, transforms, text, shadows, images,
`get/putImageData`, `toDataURL`/`toBlob`, clipping, Porter-Duff and separable blend modes, `Path2D` and
`OffscreenCanvas`. Video sources, patterns, canvas filters and `webgpu`/`bitmaprenderer` contexts remain out.

### R3.2 IntersectionObserver — done
Thresholds, `rootMargin` and per-frame delivery, with observer tests in `rendrix-js`.

### R3.3 Form controls — partial
Undo/redo, full constraint validation API, select popup and keyboard navigation, file and date pickers,
`stepUp`/`stepDown`/`valueAsDate` and native file drops are in. Open: visual validation bubble UI, range
input, script-synthesized draggable-element DnD.

### R3.4 CSS gaps — partial
Landed: `:has()`, `@supports` (real evaluation), `@layer` (reversed for `!important`), `@container` size
queries, nesting, `@import`, `calc()` with mixed units, `grid-template-areas`, `word-break`, line-clamp,
`text-overflow: ellipsis`, real blur/backdrop-filter/polygon clips, per-side border colours and dashed/
dotted borders, `:focus-visible`, font-DB cache. Open: `cqw`/`cqh`, WOFF2, `writing-mode`, spatially-varying
masks, full table (`colspan`/`rowspan`, automatic sizing) and float (text wrap, packing) fidelity.

### R3.5 Shadow DOM and custom elements — done (minimal)
Open/closed shadow roots, `customElements.define/get/whenDefined/upgrade` and connected/disconnected
callbacks. No closed-root styling APIs or full form-associated custom elements.

### R3.6 3D scenes — experimental, GPU path upgraded
`RendrixScene.createScene(...)` plus a `webgl`/`webgl2` alias rasterize JSON-described scenes into a canvas
texture; a GPU scene backend (`scene3d.rs`, `Item::Scene3D`) composites `<rendrix-scene>` / `<canvas data-3d>`
hosts. The JS→GPU bridge was repaired (it silently rejected every scene before) and the GPU forward pass now
has Depth24Plus testing, up to 4 directional + 4 point lights with range falloff, alpha blending with
back-to-front sorting, optional backface culling, frustum culling, and UV-mapped material textures
(`RendrixScene.setTexture`). Still missing: shadows, glTF, PBR, skeletal animation, instancing, and
real WebGL shaders/framebuffers. This is not a full 3D engine.

### R3.7 Web target (`rendrix-web`) — experimental
The same engine compiled to wasm runs in a Worker using the browser's JS engine. Pointer/touch/keyboard/IME,
fetch, canvas and rendering work under SwiftShader WebGPU. The renderer now has a WebGL2 fallback
(uniform-backed instances and polygon clip, selected from device limits) covered by a native
downlevel-limits pixel test; a browser run without WebGPU is still unrecorded. Open: browser-backed
WebSocket/Workers/localStorage persistence, static import cycles, and a runaway-script interrupt
(native QuickJS has a 5 s/task budget).

## Phase 4 — JavaScript and TypeScript parity

### R4.1 Direct JS support — done
`--check-js` and the `allowJs: true` / `checkJs: false` contract, an actionable dev-loader error for TS/JSX
directories, and JS/linker micro-benchmarks all landed.

### R4.2 Source maps — done
`tsc` maps stored per module in `bundle.sourceMaps` (an additive `rendrix-app/1` field); the runtime remaps
`at name (file:line:col)` frames. Invalid maps are ignored.

### R4.3 Hot reload — done
The dev loop diffs `bundle.modules`, writes `.rendrix/hot-update.json`, and the running app hot-swaps modules
in a fresh JS runtime on the live DOM. State preservation is script-cooperative; failed updates keep the
last-good tree. `--no-hot` restores restart behaviour.

## Phase 5 — Developer ecosystem

### R5.1 Framework adapters — Solid done, React open
`packages/solid-adapter` runs upstream `solid-js` on the Rendrix DOM (mount/update/cleanup/events/`Show`/`For`,
13-step headless matrix). A React reconciler and Vue adapter have not been started.

### R5.2 Devtools / inspector — snapshot done, overlay open
`rendrix-runtime --inspect out.json` dumps DOM, computed styles, layout rects, engine stats and console tail
with zero per-frame cost. No overlay UI or devtools protocol.

### R5.3 Headless rendering — done
`rendrix render <page.html|dir> -o out.png|out.pdf|frames/` produces PNG, a one-page PDF or a PNG frame
sequence (the handoff to the video pipeline).

## Phase 6 — Application capabilities (longer term)

| Task | Status |
|---|---|
| R6.1 Multi-process / security model | Not started; single-process with capability grants is the current scope. Execution limits are not a sandbox |
| R6.2 3D rendering | Experimental scene API landed (R3.6); a full 3D engine is a separate effort |
| R6.3 Media playback (`<video>`/`<audio>`) | Not started |
| R6.4 Distribution | Signing, NSIS installer, manifest and update client are wired; no signed/macOS/Linux install runs are recorded |
| R6.5 Mobile shells | Not started; `Session` scaffolding and tests are in place |

## Task ordering (current)

| Order | Task | Impact |
|---:|---|---|
| 1 | Commit the in-flight batch | Stability / risk |
| 2 | Re-measure cold start and mutation latency; publish numbers | Performance claims |
| 3 | macOS/Linux validation + manual AT pass | Real-world use |
| 4 | WebGL2 fallback and web-target API gaps | Browser target viability |
| 5 | 50k-node and second-machine benchmarks | Scaling knowledge |
| 6 | CSS/API gap list (R3.3/R3.4) | Web compatibility |
| 7 | React adapter + overlay inspector | Ecosystem |

## What Rendrix is NOT close to today

- Browser: no full HTML5 tree builder, no security/multi-process model, no navigation, no full CSS.
- IDE class app: no terminal/pty, no process model, no overlays/devtools protocol.
- Full 3D engine or video editor: no depth buffer/PBR/glTF, no media decoding, no timeline.
- Complex multi-window desktop: several windows and dialogs work; no tray or global shortcuts yet.

## Dependencies and blockers

- Cold-start and mutation work depend on trustworthy measurements, not on new architecture.
- Web-target features are independent of native features but must not regress the 1 ms hover/scroll path.
- Framework adapters beyond Solid depend on DOM fidelity (R3.3/R3.4) and stable hot reload (R4.3).
- macOS/Linux validation and the AT pass need hardware/CI runs, not code changes alone.

## Tests required per phase

- Performance: re-run `bench 1000` on a quiet machine after each change; keep the equivalence tests green.
- Platform: `tests/platform.rs`, `tests/mobile.rs`, plus recorded manual runs per OS.
- Rendering: `tests/paint_gaps.rs` and renderer pixel tests for every compositing change.
- Web platform: `rendrix-js` tests plus the curated WPT subset (`node scripts/wpt.mjs`).
- Developer ecosystem: CLI tests, hot-reload session tests, Solid adapter matrix.
