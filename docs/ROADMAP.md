# Rendrix Implementation Roadmap

Date of audit: 2026-11-14. Derived from the implementation, performance and dependency audits. Tasks are ordered by foundational impact. Status reflects verified state, not README claims.

## Guiding priorities (from ENGINE_ASSESSMENT.md, re-verified)

1. Incremental box generation (enables many app types).
2. Accessibility tree (AccessKit).
3. macOS/Linux validation.
4. Canvas 2D.
5. Framework adapters (Solid first).
6. Hot reload + source maps + inspector.
7. Cold start (font cache).

## Implementation-mission status (2026-11-14, main-agent verified;
## A3/A4/D2–D4 mission integrated — see rows below)

`cargo test --workspace` 234 passed / 0 failed / 2 ignored (documented
follow-ups); `node --test packages/cli/test` 16/16,
`packages/solid-adapter/test` 2/2; release `bench 1000`: resize 10.07 ms
(<15 goal met), mutations ~6.5–7.0 ms (<3 ms goal open — Taffy re-solve floor),
idle/hover/scroll ≤1.35 ms, display list identical (119/1/773).

- Done: R0.2 (M0), R1.1 (P1), R1.2 (P2+P3), R1.4 (G1), R2.1 (A7), R2.3
  (A2 + S1 flags), R4.1 (L1–L3), R4.2 (D1), L4 guard, A5 WebSocket, A6 fetch
  scope, A1 multi-window, G1/G2-side, P4, P5, **R3.1 Canvas 2D (A3)**,
  **workers (A4)**, **R4.3 HMR (D2)**, **R5.2 inspector (D3)**,
  **R5.1 Solid adapter (D4)**.
- Partial: R3.4 CSS gaps (`:has()`, tables/floats open), S1 distribution
  (Windows verified; macOS/Linux unvalidated — no claims).
- Blocked: R3.2 IntersectionObserver (no owner), R1.3 cold-start font cache,
  R2.2 macOS/Linux validation, R6.x (explicit non-goals).
- R0.1: the 4 pre-existing uncommitted fixes remain uncommitted (preserved
  verbatim; commit authorization was never given).

## Phase 0 — Stabilize and measure (foundation)

### R0.1 Uncommitted fix review and commit
- **Status:** In progress (4 files modified).
- **Missing:** Commit/review of fixes in `rendrix-css/src/parser.rs` (keyframes non-finite offsets), `properties.rs` (border-width), `rendrix-js/src/quickjs.rs` (array unwrap), `rendrix-runtime/src/session.rs` (element unwrap).
- **Crates/files:** as listed.
- **Proposed:** Review and land; add tests for each.
- **Build/test:** cargo test --workspace.
- **Blockers:** None.

### R0.2 Expand benchmarks (Criterion)
- **Status:** Missing.
- **Missing:** Micro-benchmarks for CSS parse/cascade, box tree build, Taffy layout, display flatten, text shaping; allocation profiling.
- **Crates/files:** `engine/crates/*/benches`, `Cargo.toml`.
- **Proposed:** Add Criterion dev-dependency and bench targets; add counting allocator feature.
- **Build/test:** cargo bench.
- **Blockers:** None.

## Phase 1 — Performance foundations (highest impact)

### R1.1 Incremental box generation
- **Status:** Missing (box tree rebuilt in full each frame).
- **Missing:** Keep `LayoutNode` per element; patch dirty subtrees instead of `boxtree::Builder::build_root()` rebuilding all.
- **Crates/files:** `rendrix-core/src/boxtree.rs`, `lib.rs` (`Engine::update` relayout branch), `rendrix-dom` dirty tracking.
- **Proposed:** Cache box nodes keyed by DOM id; regenerate only when style/children/text changed; reconcile with Taffy's incremental tree.
- **Performance:** Target: mutation frame < 3 ms (currently 6.9–8.4 ms); 120 FPS for mutations.
- **Tests:** Core tests for subtree rebuild correctness; bench before/after.
- **Blockers:** Dirty tracking must expose per-subtree change sets (already partially there via style_roots/state_roots).

### R1.2 Media-query-aware restyle
- **Status:** Partial (full restyle on viewport change, 22.7 ms).
- **Missing:** Avoid `all = true` restyle when only viewport-dependent media queries change; invalidate only affected rules/subtrees.
- **Crates/files:** `rendrix-core/src/lib.rs` (`refresh_rules`, `restyle`), `rendrix-css/src/cascade.rs`, `parser.rs` (MediaQueryList).
- **Proposed:** Track which rules contain media queries; on viewport change, mark only their affected subtrees dirty.
- **Performance:** Target: resize < 15 ms.
- **Tests:** CSS media evaluation tests; core integration.
- **Blockers:** R1.1 for subtree patching.

### R1.3 Cold-start font cache
- **Status:** Missing (0.75 s first frame).
- **Missing:** Persist system font DB to disk; lazy shape off-screen paragraphs; reuse across runs.
- **Crates/files:** `rendrix-gfx/src/text.rs` (`TextSystem::new`, `FontSystem::new`), `rendrix-core`.
- **Proposed:** Serialize font DB cache keyed by OS/font version; defer shaping to visible paragraphs; shape in background if possible (single-threaded constraint).
- **Performance:** Target: first frame < 0.2 s.
- **Tests:** TextSystem tests; visual regression.
- **Blockers:** None.

### R1.4 Glyph atlas LRU (not full clear)
- **Status:** Partial (eviction clears entire map).
- **Missing:** LRU eviction of least-used glyphs instead of full clear; larger atlas option.
- **Crates/files:** `rendrix-gfx/src/renderer.rs` (`atlas_entry`, `Atlas`).
- **Proposed:** Track usage; evict oldest entries; keep white block.
- **Performance:** Avoid one-frame stale glyphs under heavy text.
- **Tests:** Renderer pixel tests.
- **Blockers:** None.

## Phase 2 — Accessibility and platform

### R2.1 Accessibility tree (AccessKit)
- **Status:** Missing (plan exists: docs/ACCESSIBILITY_PLAN.md).
- **Missing:** Build accessibility tree from DOM; expose to OS; keyboard/screen-reader support.
- **Crates/files:** New `rendrix-a11y` or within `rendrix-core`; `rendrix-runtime/src/window.rs`.
- **Proposed:** AccessKit integration; map DOM roles/states; update on mutation.
- **Tests:** Unit tests for tree construction; OS-level manual verification.
- **Blockers:** None foundational but large.

### R2.2 macOS and Linux validation
- **Status:** Missing (untested).
- **Missing:** Build/run on macOS/Linux; fix platform-specific issues (font paths, clipboard, IME).
- **Crates/files:** `rendrix-runtime/src/window.rs`, `host.rs` (storage base path).
- **Proposed:** CI builds on all three OS; manual run on real hardware.
- **Tests:** Existing test suites on each platform.
- **Blockers:** Access to hardware/CI.

### R2.3 File dialogs, multi-window, tray, global shortcuts
- **Status:** Missing (clipboard only ported).
- **Missing:** `rfd` dialogs, multiple windows, tray, global shortcuts, updater.
- **Crates/files:** `rendrix-runtime`, `rendrix-js/host.rs` (new ops), `prelude/web.js`.
- **Proposed:** Port from legacy `native/src/*` (platform.rs, tray.rs, hotkeys.rs, updater.rs).
- **Tests:** Integration tests for dialogs/window commands.
- **Blockers:** R2.2 for non-Windows.

## Phase 3 — Rendering and web platform gaps

### R3.1 Canvas 2D
- **Status:** Missing (`getContext()` returns null).
- **Missing:** `<canvas>` backed by the same renderer; 2D drawing API.
- **Crates/files:** `rendrix-gfx` (new canvas target), `prelude/dom.js` (HTMLCanvasElement), `host.rs`.
- **Proposed:** Render to offscreen texture; expose CanvasRenderingContext2D subset.
- **Tests:** Pixel tests.
- **Blockers:** R1.1 not required.

### R3.2 IntersectionObserver
- **Status:** Missing.
- **Missing:** Observer implementation in prelude + host geometry queries.
- **Crates/files:** `prelude/web.js`, `host.rs` (rect queries).
- **Proposed:** Per-frame intersection checks against viewport/clips.
- **Tests:** Observer delivery tests.
- **Blockers:** None.

### R3.3 Form controls completion
- **Status:** Partial.
- **Missing:** Validation UI, undo, select popup, file/date/range inputs.
- **Crates/files:** `prelude/dom.js`, `rendrix-dom` form state, `session.rs` text editing.
- **Proposed:** Incremental: select popup first, then validation.
- **Tests:** Session tests for each control.
- **Blockers:** None.

### R3.4 CSS gaps
- **Status:** Partial.
- **Missing:** `:has()`, `grid-template-areas`, floats, tables, multi-column, `text-shadow`, `text-overflow: ellipsis` drawing, `calc()` mixed units, WOFF/WOFF2, per-side border styles, `filter`, `clip-path`, `@container`, `@layer` ordering, nesting, `@import`.
- **Crates/files:** `rendrix-css/*`, `rendrix-core/boxtree.rs`, `paint.rs`, `text.rs`.
- **Proposed:** Prioritize `:has()` and `text-overflow` (high web compat value); tables/floats are large.
- **Tests:** CSS unit tests per feature.
- **Blockers:** None.

### R3.5 Shadow DOM, custom elements
- **Status:** Missing.
- **Missing:** Shadow roots, custom element registry, lifecycle.
- **Crates/files:** `rendrix-dom`, `prelude/dom.js`.
- **Proposed:** Large; defer until framework adapters need it.
- **Tests:** DOM tests.
- **Blockers:** None.

## Phase 4 — JavaScript and TypeScript first-class parity

### R4.1 Direct JavaScript source support (no unnecessary TS compilation)
- **Status:** Partial (JS goes through TS transpileModule and AST collection).
- **Missing:** Skip transpilation for pure JS when no TS features; use a lighter specifier collector; allow projects without tsconfig; source maps.
- **Crates/files:** `packages/cli/src/app/build.mjs` (`compile`, `typeCheck`, `collectSpecifiers`).
- **Proposed:** Detect TS-only syntax; if none, only rewrite imports via a JS parser (or keep TS parser but skip transpile); add sourceMap option.
- **Tests:** CLI tests for .js/.mjs/.jsx projects.
- **Blockers:** None.

### R4.2 Source maps
- **Status:** Missing.
- **Missing:** Emit source maps mapping compiled JS back to TS/JS source; runtime error stack remapping.
- **Crates/files:** `build.mjs`, `rendrix-js` error reporting.
- **Proposed:** Enable ts sourceMap; store maps in bundle; remap stacks in runtime.
- **Tests:** CLI tests.
- **Blockers:** R4.1.

### R4.3 Hot reload with state preservation
- **Status:** Missing (dev rebuilds, no state preservation).
- **Missing:** Incremental rebuild + module re-evaluation preserving state.
- **Crates/files:** CLI dev loop, runtime module loader.
- **Proposed:** Re-run changed modules; preserve DOM where possible.
- **Tests:** E2E.
- **Blockers:** R1.1.

## Phase 5 — Developer ecosystem

### R5.1 Framework adapters
- **Status:** Missing.
- **Missing:** Solid adapter first (fine-grained DOM updates), then React reconciler.
- **Crates/files:** `packages/types`, examples, new adapter packages.
- **Proposed:** Solid maps directly to this DOM; React needs a reconciler.
- **Tests:** Integration tests rendering a framework app.
- **Blockers:** R1.1 (performance), R4.x.

### R5.2 Devtools / inspector
- **Status:** Missing.
- **Missing:** In-app DOM/CSS/layout inspector.
- **Crates/files:** `rendrix-runtime` (overlay), `rendrix-core`.
- **Proposed:** Overlay UI reading engine state.
- **Tests:** Manual.
- **Blockers:** None.

## Phase 6 — Application capabilities (longer term)

### R6.1 Multi-process / security model
- **Status:** Missing.
- **Missing:** Process isolation for apps; capability model hardening.
- **Proposed:** Defer; current single-process model acceptable for desktop apps.

### R6.2 3D rendering
- **Status:** Missing (renderer is 2D SDF only).
- **Missing:** 3D scene graph, meshes, cameras, lighting. This is a separate engine effort; do not confuse current GPU renderer with a 3D engine.
- **Proposed:** Only if a 3D editor/creative app is a goal; likely a new crate.

### R6.3 Media playback (video/audio)
- **Status:** Missing.
- **Missing:** `<video>`/`<audio>`, media decoding pipeline.
- **Proposed:** Integrate a media decoding library (e.g., ffmpeg bindings) later.

## Task ordering summary

| Order | Task | Impact |
|---:|---|---|
| 1 | R0.1 Uncommitted fixes | Stability |
| 2 | R1.1 Incremental box generation | Performance (all app types) |
| 3 | R1.2 Media-query restyle | Performance (resize) |
| 4 | R1.3 Cold start | Startup |
| 5 | R2.1 Accessibility | Real-world use |
| 6 | R2.2 macOS/Linux | Platform |
| 7 | R3.1 Canvas 2D | App capability |
| 8 | R4.1 JS direct support | Language parity |
| 9 | R4.2 Source maps | DX |
| 10 | R5.1 Framework adapters | Ecosystem |

## What Rendrix is NOT close to today

- Full-featured IDE (T3 Code-class): missing multi-window, file dialogs, terminals (pty), canvas, large-scale performance validation.
- Native terminal: missing pty/process APIs; text rendering present.
- Browser: missing security model, multi-process, full HTML5 parser; rendering foundation present.
- 3D editor: no 3D engine; only 2D SDF renderer.
- Video editor: no media playback, no timeline, no canvas.
- Complex multi-window desktop: no multi-window, dialogs, tray.

## Dependencies and blockers

- R1.1 depends on dirty-tracking exposure (already present).
- R1.2 depends on R1.1 for subtree patching.
- R3.x depends on R1.x only for performance, not correctness.
- R4.x independent.
- R5.1 depends on R1.1 and R4.x.

## Tests required per phase

- Phase 1: core rebuild correctness, bench before/after, no regression in 156 Rust + 9 CLI tests.
- Phase 2: accessibility tree unit tests; platform smoke tests.
- Phase 3: pixel tests for canvas; CSS feature tests.
- Phase 4: CLI tests for JS projects; source-map resolution tests.
- Phase 5: framework render integration tests.
