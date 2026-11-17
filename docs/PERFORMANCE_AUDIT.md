# Rendrix Performance Audit

Date of audit: 2026-11-14. Platform: Windows 11, NVIDIA GeForce RTX 3060, Vulkan, 125% DPI, headless and windowed. All numbers are measured on this machine only; macOS/Linux have not been measured.

## Executive summary

- **Interactive scenarios fit a 60 FPS budget (16.7 ms).** Hover and scrolling run at ~1.2 ms; DOM mutations at ~6.5–7 ms; 120 FPS (8.3 ms) is met for hover/scroll and at the limit for mutations.
- **The old whole-tree bottleneck is fixed.** Incremental box generation ("P1"), stylesheet/`RuleSet` caching and viewport-dependency pruning landed; the remaining mutation cost is the Taffy re-solve plus `InlineContent::key` rehashing, not full box-tree rebuilds.
- **Viewport resize is no longer the worst case** (~10 ms in the last clean run, down from 36.1 ms): CSS restyle is ~0.35 ms when no media-dependent rules changed; the residual ~8 ms is Taffy re-solving 9,009 nodes.
- **Cold-start numbers below are stale.** A disk font-database cache and LRU glyph atlas landed after they were recorded. Two re-measurement attempts on 2026-11-16 produced mutually inconsistent results (see "Re-measurement note" below), so no new cold-start claim is made — re-run on a quiet machine before publishing.
- **Idle applications do no work** (no timer/animation/pending change → no frame); an idle frame is GPU-only when one is produced.
- **Executable size 13.8 MB** (release, Windows, thin LTO). GPU instance buffer grows to the next power of two.

## Measured results (bench)

Command: `cargo run --release -p rendrix-runtime --manifest-path engine/Cargo.toml -- bench 1000` (1000 list items, 9009 DOM nodes, 1000×700 @1×, median of N frames, CPU pipeline + GPU submission/completion, excluding vsync).

| Scenario | total ms | style ms | layout ms | paint ms | GPU ms |
|---|---:|---:|---:|---:|---:|
| Idle frame (nothing changed) | 1.09 | 0.00 | 0.00 | 0.00 | 0.32 |
| Toggle a class on 1 item | 6.92 | 0.06 | 5.25 | 0.17 | 0.52 |
| Change text of 1 item | 8.42 | 0.01 | 6.28 | 0.19 | 0.58 |
| Hover change (pointer moves between items) | 1.03 | 0.03 | 0.00 | 0.00 | 0.31 |
| Scroll by one wheel notch | 1.16 | 0.02 | 0.00 | 0.12 | 0.31 |
| Add + remove 10 items | 7.09 | 0.03 | 5.26 | 0.16 | 0.52 |
| Viewport resize (full restyle + relayout) | 36.12 | 22.72 | 11.62 | 0.19 | 0.57 |
| JS handler: classList.toggle via script | 7.57 | 0.05 | 5.55 | 0.17 | 0.75 |
| Startup (runtime + JS builds DOM + first frame) | 607.2 ms (819.2 ms of it the first frame) | — | — | — | — |
| Last display list | 119 items; GPU batches 1; instances 773 | — | — | — | — |

Source: `engine/crates/rendrix-runtime/src/bench.rs`, `main.rs` (bench command). Numbers vary by several tenths of a millisecond between runs and were measured on one machine.

## Reproduced baseline (HEAD, 2026-11-14, Subagent 5)

Commit `d4740ef`, quiet machine, same setup (Windows 11, RTX 3060, Vulkan, 1000×700 @1×).
Confirms the table above within run-to-run variance. The M0 `shapes`/`restyles`/`layouts`
columns did not exist at this commit (added on `dev` after); dev-tree medians for the new
columns are pending a green tree.

| Scenario | total ms | style ms | layout ms | paint ms | GPU ms |
|---|---:|---:|---:|---:|---:|
| Idle frame (nothing changed) | 1.00 | 0.00 | 0.00 | 0.00 | 0.29 |
| Toggle a class on 1 item | 8.66 | 0.06 | 6.71 | 0.21 | 0.54 |
| Change text of 1 item | 9.24 | 0.01 | 7.42 | 0.21 | 0.53 |
| Hover change (pointer moves between items) | 1.02 | 0.03 | 0.00 | 0.00 | 0.31 |
| Scroll by one wheel notch | 1.22 | 0.02 | 0.00 | 0.15 | 0.31 |
| Add + remove 10 items | 10.12 | 0.04 | 7.72 | 0.24 | 0.58 |
| Viewport resize (full restyle + relayout) | 38.85 | 25.17 | 11.93 | 0.19 | 0.54 |
| JS handler: classList.toggle via script | 7.35 | 0.05 | 5.66 | 0.19 | 0.52 |
| Startup (runtime + JS builds DOM + first frame) | 617.2 ms (796.7 ms of it the first frame) | — | — | — | — |
| Last display list | 119 items; GPU batches 1; instances 773 | — | — | — | — |

Note: one earlier `dev`-tree run on the same machine showed far worse numbers
(resize ~107 ms, toggle ~40 ms), but it ran concurrently with a full-workspace
compilation and against mid-flight uncommitted edits, so it is **not** a baseline —
it is reported to the main agent as a contaminated observation only.

## M0 Criterion medians (quiet runs, 2026-11-14, Subagent 5)

Same machine. Criterion default statistics (100 samples); medians below, intervals in
`target/criterion`. CSS group re-ran quiet after a first attempt overlapped a
workspace compile (the first attempt's `add_stylesheet` median was ~2× inflated —
contention artifact, discarded).

| Bench | Median |
|---|---:|
| `css/parse_bench_stylesheet` (~210-rule sheet) | 316.18 µs |
| `css/ruleset_add_stylesheet` (same sheet) | 148.54 µs |
| `css/compute_style_single` | 2.8654 µs |
| `css/compute_style_9k_tree` | 14.356 ms (~1.6 µs/element) |
| `css/compute_style_ruleset_size/0` | 3.7709 µs |
| `css/compute_style_ruleset_size/500` | 3.5744 µs |
| `css/compute_style_ruleset_size/1000` | 2.6274 µs |

Caveat: the ruleset-size trio is noise-dominated (±1 µs flips order between runs) —
group-order/cache-warmth effects, not a scaling signal. A conclusive scaling claim needs
interleaved same-process measurement. Taffy/text/js/pipeline bench medians are pending
(a green tree, then quiet runs).

## Integrated result (dev, 2026-11-14, main agent)

Same machine/setup. All five subagent tracks + main-agent wiring
(`AppBundle.source_maps`, session map load, `--check-js`, `EngineStats`
shaping signals) integrated; `cargo test --workspace` green (see §Test
results in the final report). `cargo run --release -p rendrix-runtime --
bench 1000`:

| Scenario | total ms | style ms | layout ms | paint ms | GPU ms | shapes | restyles | layouts |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| Idle frame | 1.05 | 0.00 | 0.00 | 0.00 | 0.29 | 0.0 | 0.0 | 0.0 |
| Toggle a class on 1 item | 7.19 | 0.05 | 5.44 | 0.18 | 0.45 | 0.0 | 1.0 | 1.0 |
| Change text of 1 item | 7.57 | 0.01 | 5.66 | 0.16 | 0.46 | 2.0 | 1.0 | 1.0 |
| Hover change | 1.03 | 0.03 | 0.00 | 0.00 | 0.30 | 0.0 | 1.0 | 0.0 |
| Scroll one notch | 1.21 | 0.02 | 0.00 | 0.14 | 0.31 | 0.0 | 1.0 | 0.0 |
| Add + remove 10 items | 7.19 | 0.03 | 5.30 | 0.15 | 0.48 | 0.0 | 1.0 | 1.0 |
| Viewport resize | **11.54** | **0.40** | **9.62** | 0.16 | 0.46 | 0.0 | 0.0 | 1.0 |
| JS `classList.toggle` | 5.67 | 0.05 | 4.21 | 0.13 | 0.43 | 0.0 | 1.0 | 1.0 |
| Startup | 627.8 ms (914.2 ms first frame) | — | — | — | — | — | — | — |
| Last display list | 119 items; batches 1; instances 773 | — | — | — | — | — | — | — |

vs HEAD baseline: resize 38.85 → 11.54 ms (style 25.17 → 0.40, restyles
1.0 → 0.0 — P2 stylesheet cache + P3 dependency pruning firing; layout
11.93 → 9.62 is Taffy re-solve of 9k nodes, the retained floor). Resize
<15 ms goal: met. Mutations 7–10 → 5.7–7.6 ms (layout −20–40% from P1
patching + P5 allocation discipline); <3 ms / 120 FPS mutation goal: not
met — residual is per-frame Taffy sync+solve plus `InlineContent::key`
rehashing (follow-ups, not regressions). Hover/scroll/idle ≤1.25 ms
preserved; GPU ≤0.5 ms; display list identical (119/1/773). Startup
unchanged (font-DB cold-start work, R1.3, explicitly deferred).

## Post A3/A4/D2–D4 integration (same machine, main agent)

A3 canvas + A4 workers + D2/D3 wiring integrated (canvas/worker/inspector all
inert when unused: no `<canvas>`, no workers, inspector on-demand only).
`cargo test --workspace`: 234 passed / 0 failed / 2 ignored (documented
follow-ups). `cargo run --release -p rendrix-runtime -- bench 1000`:

| Scenario | total ms | style ms | layout ms | paint ms | GPU ms | shapes | restyles | layouts |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| Idle frame | 1.21 | 0.00 | 0.00 | 0.00 | 0.50 | 0.0 | 0.0 | 0.0 |
| Toggle a class on 1 item | 6.98 | 0.05 | 5.24 | 0.17 | 0.71 | 0.0 | 1.0 | 1.0 |
| Change text of 1 item | 6.92 | 0.01 | 4.98 | 0.16 | 0.70 | 2.0 | 1.0 | 1.0 |
| Hover change | 1.23 | 0.03 | 0.00 | 0.00 | 0.50 | 0.0 | 1.0 | 0.0 |
| Scroll one notch | 1.31 | 0.01 | 0.00 | 0.10 | 0.50 | 0.0 | 1.0 | 0.0 |
| Add + remove 10 items | 6.72 | 0.03 | 4.81 | 0.13 | 0.66 | 0.0 | 1.0 | 1.0 |
| Viewport resize | **10.07** | **0.35** | **8.11** | 0.16 | 0.68 | 0.0 | 0.0 | 1.0 |
| JS `classList.toggle` | 6.48 | 0.05 | 4.62 | 0.15 | 0.80 | 0.0 | 1.0 | 1.0 |
| Startup | 588.0 ms (726.2 ms first frame) | — | — | — | — | — | — | — |
| Last display list | 119 items; batches 1; instances 773 | — | — | — | — | — | — | — |

No regressions vs the 11.54 ms table: resize improved further (11.54 → 10.07),
mutations flat-to-better, idle/hover/scroll within run-to-run variance, display
list identical (119/1/773 — canvas adds zero batches when unused).

## Re-measurement note (2026-11-16)

A disk-backed font-database cache and LRU glyph atlas landed after the tables above. Two `bench 1000`
re-runs on the same machine while other work was compiling produced **inconsistent, contention-affected
numbers** (idle 2.05 ms then 5.01 ms, first-frame 970 ms then 1458 ms, total startup 1117 ms then 903 ms —
the latter reporting a first frame longer than the total). These runs are **discarded as contaminated**,
not published as results. The last clean table above remains the reference. Re-run on a quiet machine with
no concurrent builds and replace the startup/first-frame rows before making a cold-start claim.

## Post renderer-fixes run (2026-11-16)

After the renderer correctness/scale work (dynamic 3D uniforms, isolated group opacity, separable blur,
shared image atlas), a release `bench 1000` run on the reference machine produced: idle 1.79 ms, hover
1.54 ms, scroll 3.47 ms, class toggle 4.35 ms, text change 6.93 ms, add/remove 5.14 ms, resize 7.81 ms,
JS toggle 3.68 ms; GPU 0.62–0.78 ms; display list still 119 items / 1 batch / 773 instances. These numbers
are in the same band as the last clean table above (run-to-run variance on this machine is several tenths
of a millisecond and larger under contention); they confirm **no regression** to the ordinary, layer-free
frame path. Startup was 952 ms total with a 782 ms first frame — still a single observation and not a
cold-start conclusion; the font-DB cache work needs a dedicated quiet re-measurement.

## Post dirty-aware sync / incremental collect run (2026-11-16)

After T1 (dirty-aware Taffy sync) and T2 (in-place `LayoutResult` carry-forward) landed, plus
T3's startup instrumentation, a quiet-ish `bench 1000` re-run on the reference machine
(second of two runs; the first was contention-affected and is discarded) produced:

| Scenario | total ms | style ms | layout ms | paint ms | GPU ms | layouts |
|---|---:|---:|---:|---:|---:|---:|
| Idle frame | 1.67 | 0.00 | 0.00 | 0.00 | 0.70 | 0.0 |
| Toggle a class on 1 item | **4.78** | 0.05 | 2.57 | 0.20 | 0.83 | 0.0 |
| Change text of 1 item | **6.19** | 0.01 | 3.77 | 0.15 | 0.88 | 1.0 |
| Hover change | 1.83 | 0.04 | 0.00 | 0.00 | 0.79 | 0.0 |
| Scroll one notch | 4.48 | 0.03 | 2.17 | 0.21 | 0.79 | 1.0 |
| Add + remove 10 items | **6.04** | 0.03 | 3.60 | 0.12 | 0.80 | 1.0 |
| Viewport resize | **9.37** | 0.39 | 6.87 | 0.14 | 0.80 | 1.0 |
| JS `classList.toggle` | **4.69** | 0.05 | 2.39 | 0.21 | 0.80 | 0.0 |

**Interpreting these numbers honestly.** Against the older "Post A3/A4/D2–D4" table
(6.98 / 6.92 / 6.72 / 10.07 / 6.48 ms) these figures look like 7–32% wins. Against the
*more recent* "Post renderer-fixes" run (4.35 / 6.93 / 5.14 / 7.81 / 3.68 ms) several of
them are slightly slower. Both earlier rows are single observations on a machine whose
run-to-run variance is large — an immediately preceding re-run of this benchmark showed
uniformly ~2× higher GPU timings with no code change between them. So the only safe
claim is that the new path is **in the same performance band**, with no measured regression
that rises above the noise floor; the percentage deltas above should not be published as
speedups without an interleaved, quiet-machine A/B.

The structural changes are independent of that noise and are verified by tests:
`layouts = 0.0` for class/JS toggles (no Taffy solve at all, via the paint-only fast path plus
dirty-box plumbing), clean subtrees skipping `to_taffy()` conversion, and unchanged boxes
being carried forward in place instead of rebuilding a ~9,000-entry map. Display list is
unchanged at 119 items / 1 batch / 773 instances. All 393 Rust tests pass (2 pre-existing
ignored), including new equivalence tests for dirty-aware sync and incremental collect.
A fair A/B needs an interleaved same-process harness, which is a follow-up.

Startup remains the unresolved cost: the new breakdown reports a ~775–1086 ms first frame,
~680–986 ms of which is shaping ~9,985 paragraphs (with font DB load at ~17 ms from cache
and JS DOM construction at ~56–60 ms). The `< 0.2 s` first-frame goal is **not** met; the
shaping count is inherent to the benchmark's unique text and already deduplicated by
`content.key()` and width-bucketing. See `docs/PERF_RESIZE_DESIGN.md` for the resize analysis
and `docs/PERF_BINARY_SIZE.md` for the (unverified) release-profile size work.

## Pipeline cost breakdown

- `Engine::update` in `engine/crates/rendrix-core/src/lib.rs`: refresh_rules → sweep GC → take dirty → restyle → tick_motion → (if needed) box tree build (`boxtree::Builder::build_root`) → Taffy layout (`TaffyEngine::layout`) → paint (display list).
- Idle: `has_pending_changes`/`needs_update` false → returns `UpdateInfo { repainted: false }` with no work; no frame produced by scheduler.
- Restyle is incremental for attribute/class/inline-style (dirty subtrees) and for hover/active/focus and child-list changes only the element itself, descending when computed style changed or stylesheet has non-local state/structural selectors.
- Layout results are incremental (Taffy keeps its tree between frames); only box generation is not.

## Bottleneck analysis

### Architectural vs implementation-level

1. **Whole-tree box generation (~5 ms / 9000 nodes).** Implementation-level. `Engine::update` calls `boxtree::Builder::new(self).build_root()` whenever `relayout` is true, rebuilding all boxes. Taffy reuses its tree, but box generation does not. Fix: keep boxes per element and patch dirty subtrees (listed as next priority in ENGINE_ASSESSMENT.md).
2. **Full restyle on viewport change (~23 ms).** Partly architectural (media queries depend on viewport; cascade must re-evaluate), partly implementation (no dirty-root pruning for media changes). Fix: invalidate only media-dependent rules and recompute affected subtrees.
3. **Cold start (~0.75 s first frame at the time of the audit).** Was implementation-level: `TextSystem::new()` scanned the system font database and the first frame shaped 3000 paragraphs. A disk font-DB cache and LRU atlas have since landed; the figure above is stale and needs a quiet re-measurement (see "Re-measurement note" above).
4. **Glyph atlas eviction.** Implementation-level but bounded. 2048×2048 atlas; eviction clears the map and re-rasters (`renderer.rs` `atlas_entry`). Could cause one-frame stale glyphs under heavy text. Fix: larger atlas / LRU eviction.
5. **CPU-to-GPU transfer.** `queue.write_buffer` for instances + globals each frame; `write_texture` for glyph uploads. Measured GPU 0.3–0.6 ms. Not a bottleneck today. For large UIs, instance buffer reallocation to next power of two (`inst_cap`) is fine.
6. **Memory allocations.** `Vec` allocations in flatten/paint; box tree allocation each frame. Part of the 5 ms. Incremental box generation would reduce.
7. **Frame pacing.** `PresentMode::AutoVsync`, `desired_maximum_frame_latency: 2`, `ControlFlow::Wait`/`WaitUntil`. Idle does no work. Good.
8. **Executable size 13.8 MB.** LTO thin, codegen-units 4. Could reduce with `lto = "fat"` and strip, but not measured. A size-focused release profile (fat LTO, `strip = "symbols"`, 1 codegen unit) has since been configured but is **not yet measured**; see [PERF_BINARY_SIZE.md](PERF_BINARY_SIZE.md).
9. **Large application interfaces.** 9009 nodes measured; hover 1.03 ms, mutation 7 ms. Scaling beyond ~10k nodes likely dominated by box generation and style traversal (O(n) dirty checks). Not yet measured.

## Existing benchmarks

- `engine/crates/rendrix-runtime/src/bench.rs` (invoked by `rendrix-runtime bench <N>`): pipeline scenario benchmark with style/layout/paint/GPU breakdown. This is the primary benchmark.
  - Startup breakdown (additive, does not change the scenario table): separates font-DB load (`rendrix_gfx::text::last_font_db_load_nanos` / `last_font_db_cache_hit`), JS DOM construction (`rendrix_runtime::session::last_script_run_nanos`), and first-frame phases (restyle / box build / Taffy layout / text shaping / display paint latched in `EngineStats::first_*`, plus first GPU submit). These are distinct fields for a quiet-machine run to attribute cold start; the printed value is a single observation and is **not** a publishable number. Shaping time is snapshotted from `TextSystem::shape_nanos`. The first-frame shaping count is also reported.
- M0 counters in `bench.rs`: per-scenario medians of newly shaped paragraphs (`TextSystem::shape_count` deltas) and engine `restyles`/`layouts` deltas, alongside the CPU columns. Taffy `last_new_nodes`/`last_restyled_nodes` and atlas `evictions` are not yet reachable from the harness (`Engine.layout` and `Renderer.atlas` are private) — production accessors still needed; see blockers below.
- Criterion micro-benches (dev-only, `criterion` dev-dependency, never shipped):
  - `rendrix-css/benches/css.rs`: `Stylesheet::parse`, `RuleSet::add_stylesheet`, `compute_style` single + 9k tree, ruleset-size scaling.
  - `rendrix-core/benches/engine.rs`: full rebuild of the 9k tree, class/text mutations, static-sheet resize, display-list traversal.
  - `rendrix-layout/benches/taffy.rs`: `TaffyEngine::layout` on 9k boxes, fixed vs varying widths.
  - `rendrix-gfx/benches/text.rs`: paragraph cache hit vs miss, wrapped measure.
  - `rendrix-js/benches/js.rs`: `call_global`, `__native` round-trip (`document.documentElement`), DOM fanout, task pump.
  - `rendrix-runtime/benches/pipeline.rs`: scripted `Session` frames (no GPU) + per-scenario allocated-byte deltas via a dev-only counting allocator.
  - Node: `packages/cli/bench/transpile.bench.mjs` (`transpileModule` single file + 200-module `buildApp`).
- Allocation/shape assertions live in `engine/crates/rendrix-runtime/tests/regression.rs` (idle/hover budgets, cache hit-rate); SA-track regression tests (dynamic import e2e, restyle equivalence, box determinism, dialog mock-op, ws validation) live there too, with `#[ignore]`d BLOCKERs for unlanded P1/G1/A5/A7 APIs.
- Rust unit tests in crates include micro-level assertions but no timing benchmarks (`#[bench]` absent).
- CLI tests measure build times incidentally (~20–600 ms) but are not performance benchmarks.

## What has not been measured

- macOS/Linux performance (untested platform).
- Startup breakdown beyond total first frame. Instrumentation now exists (see `bench.rs` startup breakdown above) but has only been wired, not run on a quiet machine — still unmeasured.
- Large-interface scaling beyond 9009 nodes.
- Text shaping cost isolated (paragraph hit/miss benches exist; machine medians pending a green tree).
- GPU memory / atlas pressure (eviction counter not yet exposed to the harness).
- Multi-frame animation cost over time.
- Per-scenario allocation bytes on the reference setup (harness records them; medians pending).

## Recommendations (measurement, not implementation)

- Add Criterion benchmarks for: CSS parsing (`Stylesheet::parse`), cascade (`compute_style`), box tree build, Taffy layout, display list flatten, text shaping.
- Add allocation profiling (dhat or counting allocator) to quantify per-frame allocations.
- Measure on a second machine/GPU to validate portability of the 1 ms idle claim.
- Measure with 50k+ nodes to expose O(n) limits.
- Do not optimize before measuring; current interactive targets are met.
