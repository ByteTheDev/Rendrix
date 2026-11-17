# Resize performance: root cause and design options

Status: research/design only. No engine source was modified. All claims about Taffy
internals were verified against `taffy 0.9.2` source in the local cargo registry
(`C:\Users\spoil\.cargo\registry\src\index.crates.io-*\taffy-0.9.2`), which is the
version resolved in `engine/Cargo.lock` (the manifest range is `0.9`, lock pins
`0.9.2`). Numbers cited from `PERFORMANCE_AUDIT.md` are measurements on one machine
and vary by several tenths of a ms; treat speedups below as hypotheses to measure.

---

## 1. Precise root cause

A viewport resize is ~8–10 ms of layout (`PERFORMANCE_AUDIT.md`: resize 10.07 ms
total, 8.11 ms layout; later run 7.81 ms total). Style is ~0.35 ms because the P2/P3
work correctly proves that a static stylesheet has no viewport-dependent computed
styles. The retained cost is a full Taffy re-solve of 9,009 nodes. Five mechanisms
compound; only the first is the one named in the task, and it is **not** by itself a
"whole-tree dirty" mechanism — the propagation is subtler and matters for the design.

### 1.1 The root box bakes viewport width into its style

`Engine::update` forces the root box width to the viewport width in two places:

- `lib.rs:427–432` (reused-tree / paint-only-fast-path fallback),
- `lib.rs:494–501` (full layout path, and the fresh-build path used at startup),

and `boxtree::refresh_root_width` (`boxtree.rs:879–886`) does the same when a reused
tree survives a resize (`lib.rs:482–484`). Concretely:

```rust
let mut s = (*tree.style).clone();
s.width = rendrix_css::Dim::Px(self.viewport.0);
tree.style = Arc::new(s);
```

`set_viewport` (`lib.rs:164–170`) sets `viewport_changed`, which forces
`layout_needed = true` (`lib.rs:390–396`) and routes every resize down the full
layout path. On that path `TaffyEngine::sync` (`taffy_engine.rs:45–104`) compares the
new root Taffy `Style` against the cached one; the width differs, so
`tree.set_style(root_node, …)` runs (`taffy_engine.rs:65–69`).

### 1.2 What `set_style` actually dirties — and what it does not

In Taffy 0.9.2, `TaffyTree::set_style` → `mark_dirty(node)` →
`NodeData::mark_dirty()` clears that node's `Cache`, then recurses **upward through
`parents`** to every ancestor (`taffy_tree.rs:869–893`, `compute/mod.rs:...`). It does
**not** clear descendants. So the common shorthand "resize dirties the whole tree"
is inaccurate at the flag level: only the root's cache is cleared. The full re-solve
happens for a different reason:

`compute_layout_with_measure` is called with a fresh
`avail = Size { Definite(viewport.0), Definite(viewport.1) }`
(`taffy_engine.rs:123–124`). Taffy's per-node cache is keyed on
`(known_dimensions, available_space, run_mode)` (`cache.rs:109–153`). For a block root,
`compute_root_layout` derives `known_dimensions.width` from the baked style width
(`compute/mod.rs:88–114`), so on resize **both** the root's `known_dimensions.width`
and its `available_space.width` change → root cache miss → root re-solves. The root's
new content-box width is then the available space handed to its children, so every
child whose width is not fully fixed (`auto`, `%`, `fr`, `flex-grow`, cross-axis
`stretch`) sees a changed cache key and misses too. That is the cascade.

### 1.3 Taffy's final-layout cache has a single slot, so alternating widths thrash

`Cache::store` for `RunMode::PerformLayout` unconditionally overwrites a single
`final_layout_entry` (`cache.rs:163–167`). The benchmark alternates
1000.0 / 960.0 (`bench.rs:153–157`, `engine.rs:100`). Each frame overwrites the only
final-layout result with the other width's geometry, so returning to a previously
seen width is still a miss. `cache_get` for `PerformLayout` does skip the available
space check when `known_dimensions` are `Some` (`cache.rs:118–131`), which is why
*fully fixed-size* subtrees would already be reused — see §2.

### 1.4 Work paid on every node regardless of cache state

- `sync` (`taffy_engine.rs:45–104`) builds a fresh Taffy `Style` via `to_taffy` for
  every one of the 9,009 nodes and does a full `Style` `PartialEq` (`c.style != style`).
  On a static-sheet resize, 9,008 of those comparisons are guaranteed equal — the
  conversion/allocation is the cost, not the dirty count (`last_restyled_nodes` would
  be 1). `to_taffy` allocates `Vec`s for grid tracks/areas and builds many small
  `Dimension`/`LengthPercentage` values; `Style` is a large struct.
- `compute_layout_with_measure` unconditionally calls `round_layout`, which walks the
  entire tree once and writes `final_layout` for every node (`compute/mod.rs:207–247`,
  called at `taffy_tree.rs:915–917`). This is O(n) but cheap relative to sizing math.
- `collect` (`taffy_engine.rs:155–173`) reads `tree.layout()` for every node — O(n)
  hash lookups, unavoidable if all boxes are needed.

### 1.5 The two-pass `calc()` relayout

`layout` (`taffy_engine.rs:128–147`) detects `has_calc_in_tree(root)`, clones the whole
`LayoutNode` tree with `resolve_tree_calcs` (O(n) `Arc`/`Vec` allocations), re-runs
`sync`, and re-runs `compute_with_measure`. For a calc-bearing tree a resize costs two
full solves. The benchmark tree has no `calc()`, so this is **not** part of the 8 ms;
it is a separate follow-up lever (§3.6).

**Summary of the causal chain:**
`set_viewport` → root style width changes → `set_style(root)` clears root cache;
the new `Definite(viewport.0)` available space misses the root's single cache entry;
the changed root content-box width misses every non-fixed descendant's cache key;
the single-slot final-layout cache cannot remember a previously seen width; meanwhile
all 9k styles are rebuilt for comparison. Result: a full solve on every resize.

---

## 2. Is "cache viewport-independent subtrees" viable in Taffy 0.9.2?

**Verdict: not as a new subsystem — Taffy already does it, and the public API cannot
do better.** This is the skeptical core of the design.

### 2.1 Taffy already caches fixed-size subtrees, per node, for free

For `RunMode::PerformLayout`, `Cache::get` returns a hit when `known_dimensions`
match (or equal the cached size) and **skips the available-space equality check
whenever a known dimension is present** (`cache.rs:118–131`). Therefore a node whose
width and height are both fixed px will hit its cache across a resize even though its
available space changed. If such a node is the root of a subtree with a fixed content
box, all descendants see an invariant available space and also hit. In other words,
a fully viewport-independent island is *already* reused by Taffy today, provided no
ancestor between it and the root changes size.

The 9k benchmark tree is the opposite case: `#list` is a flex column, `.item` is
flex with cross-axis stretch, `.label` is `flex:1` — all widths depend on the root's
width. There is nothing viewport-independent to cache, so this optimization would
not change the benchmark at all.

### 2.2 There is no public subtree-freeze API

Taffy stores results in `NodeData.unrounded_layout` / `final_layout` and mutates them
through the private `LayoutPartialTree` trait (`set_unrounded_layout`,
`set_final_layout`), exposed only through the internal `TaffyView`
(`taffy_tree.rs:901–937`). `TaffyTree` exposes no "snapshot subtree layout" or
"inject layout" operation. To freeze a subtree you would have to (a) copy out every
node's `Layout` after a solve, (b) skip that subtree on the next solve, and (c) write
the saved layouts back — which requires private APIs or forking Taffy. Moreover, a
frozen layout is only valid while its available space is invariant — exactly the
condition under which §2.1 says the built-in cache already hits. Freezing therefore
buys nothing on correctness grounds and costs a fork. **Reject as primary lever.**

### 2.3 Better levers, ranked

| Lever | What it does | Risk | Expected win | Recommend? |
|---|---|---|---|---|
| **L1 — short-circuit `sync` on style Arc identity** | Store source `Arc<ComputedStyle>` (+ parent-areas signature) in `Cached`; skip `to_taffy` and `Style` compare for unchanged nodes | Low | 0.5–2 ms/frame (hypothesis; measure) | **Yes, first** |
| **L2 — small LayoutResult cache keyed by (viewport, tree revision)** | Return a saved `LayoutResult` for a repeated width if DOM/styles/text unchanged | Medium (invalidation correctness, memory) | Large on alternating-width bench; ~0 on monotonic resize storms | Conditional |
| **L3 — stop baking viewport width into reused root style** | Drop `set_style(root)`/`mark_dirty(root)` on resize; rely on available space | Medium (root `display` semantics) | Small; mostly removes one dirty event | Only with equivalence tests |
| **L4 — viewport-independence analysis, skip Taffy entirely** | Detect fixed-only layout and reuse old result + patch root box | High | 0 on the benchmark (flex tree fails the test) | No |
| **L5 — disable Taffy rounding** | Set `use_rounding=false`, round in `collect` | Medium (visual correctness) | ~0.1–0.3 ms | No |
| **L6 — kill second calc pass** | Resolve calc without full re-sync | Medium | Large only on calc trees | Separate follow-up |

**Primary recommendation: L1, measured in isolation.** If the measurement shows the
residual is still dominated by Taffy sizing (as the audit implies), L2 is the only
lever that can make the *alternating-width* benchmark fast; it must be presented as
narrow, not as a general resize fix. L3/L4 are not worth the correctness risk for the
target workload.

### 2.4 L1 design detail (concrete)

In `taffy_engine.rs`, extend `Cached` (line 18) with the source style identity and the
parent-area dependency, and restructure `sync` (line 45):

```rust
struct Cached {
    node: NodeId,
    style: Style,                // Taffy style (still needed for change detection)
    css_style: Arc<ComputedStyle>, // NEW: identity of the source style
    areas_sig: u64,              // NEW: hash of containing-block grid areas
    kind_sig: u64,
    children: Vec<u64>,
    seen: bool,
}
```

At the top of `sync`, compute `kind_sig` as today, compute `areas_sig` from
`parent_areas`, and check:

```rust
let unchanged = self.nodes.get(&n.id).map_or(false, |c| {
    Arc::ptr_eq(&c.css_style, &n.style)
        && c.areas_sig == areas_sig
        && c.kind_sig == kind_sig
});
let style = if unchanged {
    None // reuse cached Taffy style; skip to_taffy and the Style compare
} else {
    Some(to_taffy(&n.style, &n.kind, parent_areas))
};
```

Then only call `tree.set_style` / bump `last_restyled_nodes` when `style` is `Some`
and differs, and always refresh `css_style`/`areas_sig` on the cached record when a
conversion happened. Correctness invariant: **equal source Arc + equal areas signature
+ equal kind signature ⇒ identical `to_taffy` output.** `ComputedStyle` is immutable
behind `Arc`; `to_taffy` depends only on style content, `parent_areas` and `kind`, so
the invariant holds. Second-pass `resolve_tree_calcs` creates fresh `Arc`s, so the
calc pass correctly re-converts every node.

This changes `last_restyled_nodes` semantics only in that it stops counting
provably-equal styles — exactly what the counter is meant to measure.

### 2.5 L2 design detail (concrete, conditional)

In `Engine` (`lib.rs:56–100`) add a cache of at most N (e.g. 4) entries keyed by
`(viewport.0, viewport.1, viewport_scale, revision)` where `revision` is a monotonically
increasing counter bumped on any DOM mutation, restyle, text change, or container-size
change — i.e. any event that already sets one of the `dirty`/`rules_dirty` flags. Value
is a cloned `LayoutResult` (9009 `BoxLayout`s ≈ 360 KB/entry; 4 entries ≈ 1.4 MB).

In `update`, immediately after `vp_changed` is read and before the full layout path
(`lib.rs:397–460`), if the key is present and `revision` matches, assign
`self.layout_result = cached.clone()` and skip `self.layout.layout(...)` entirely.
On a miss, run layout as today and insert the result.

Risks that must be handled: (a) `revision` must cover *all* inputs to layout — text
measurement depends on the font/glyph state, replaced-image intrinsic sizes, and
`dyn` style changes; (b) `@container` size changes force a rule rebuild on the next
frame (`lib.rs:511–519`), so a cached geometry could be stale across that boundary;
(c) L2 must be disabled while animations run. Because a real resize storm produces
mostly new widths, L2's real-world value is low; it should only be accepted if the
team explicitly targets repeated-width scenarios.

---

## 3. Feasibility and risk analysis

### 3.1 What breaks if you try to skip re-layout by feature

| Feature | Depends on viewport width? | Consequence for any "skip re-solve" scheme |
|---|---|---|
| `%` width / height | Yes, via containing block | Must re-solve; cannot freeze |
| `fr` / `%` grid tracks; `minmax` with `%` | Yes | Must re-solve the grid container and ALL its items |
| `flex-grow` / `flex:1` / cross-axis `stretch` | Yes | Must re-solve the flex line; this is the benchmark's exact pattern |
| `calc()` | Yes, two-pass against containing block | Both passes must re-run (§1.5); freezing one pass is invalid |
| Absolute positioning with `%` insets / `right`/`left` | Yes | Must re-solve; note sticky insets are zeroed in `to_taffy` (`taffy_engine.rs:470–477`), so sticky is an exception |
| Auto width / shrink-to-fit / intrinsic text | Partly: text wrap width comes from available space | Text `measure` is called with a different `max_width`, so the paragraph cache misses too — not just Taffy |
| Fixed px sizes only | No | Safe; Taffy already caches these (§2.1) |

### 3.2 Risks specific to L1

- Parent `grid-template-areas` feed `to_taffy` via `parent_areas` (`taffy_engine.rs:57`,
  `528–539`). If a parent's areas change without its style Arc changing — impossible
  today because areas live inside `ComputedStyle`, so a change produces a new Arc — the
  `areas_sig` guard still makes this safe. Keep the guard.
- `LayoutKind::Text` uses `kind_sig = content.key() | 1` (`taffy_engine.rs:49`);
  `InlineContent::key` is memoized, so this stays cheap and is already part of the
  proposed signature.
- Any future code that mutates a `ComputedStyle` in place behind a shared Arc would
  silently break the invariant. Add a comment asserting immutability, and a unit test
  that rebuilds styles with equal content in fresh Arcs (different pointers) and checks
  that layout is still correct (it will just re-convert).

### 3.3 Risks specific to L2

- Stale geometry if `revision` misses an input (font load completing, image natural
  size arriving, DPR/scale change, dark-mode toggle). Today all of these funnel through
  flags that force layout, so tie `revision` to `layout_needed` rather than trying to
  enumerate causes.
- Memory: 4 × 360 KB = ~1.4 MB resident; acceptable for a renderer, but expose a
  toggle and default small.
- Test equivalence must cover "resize to width W via cache hit" vs "fresh engine at
  width W" for all 9,009 boxes.

---

## 4. Phased implementation plan (specific files/functions)

All paths below are in YOUR COPY at
`C:\Users\spoil\AppData\Local\Temp\opencode\rendrix-agents\t4`. Another agent owns the
actual edits; this is the hand-off plan.

**Phase 0 — instrument before changing anything.**
- Expose `TaffyEngine.last_new_nodes` / `last_restyled_nodes` through `Engine` (audit
  `PERFORMANCE_AUDIT.md:179` lists this as a blocker). Add accessors in
  `rendrix-core/src/lib.rs` near `Engine::layout_size` (line 618) and thread them into
  `rendrix-runtime/src/bench.rs` `Sample` (line 42) and the printed table (line 166).
- Add a Criterion timing around only `TaffyEngine::layout` inside
  `rendrix-layout/benches/taffy.rs` (separate from `Engine::update`) to separate Taffy
  solve from `sync` + `collect`. This decides whether L1 or L2 is worth doing.

**Phase 1 — L1 (style-identity short-circuit).**
- `engine/crates/rendrix-layout/src/taffy_engine.rs`: extend `Cached` (line 18), rework
  `sync` (line 45), keep `layout` (line 108) unchanged. Add a unit test next to
  `unchanged_tree_restyles_nothing…` (line 674) that rebuilds equal-content styles in
  fresh `Arc`s and asserts identical `LayoutResult`.
- Expected observable: `last_restyled_nodes` stays 1 on static-sheet resize; layout ms
  drops by the measured sync cost; hover/scroll/mutation unchanged or better.

**Phase 2 — L2 (LayoutResult cache), only if Phase 0 shows Taffy sizing dominates.**
- `engine/crates/rendrix-core/src/lib.rs`: add the cache field + revision counter near
  the other `Engine` fields (lines 60–110); hook the hit/miss into `update` after
  `vp_changed` is captured (line 397) and before `self.layout.layout(...)` (lines
  485/500); bump revision wherever `dirty.*`, `rules_dirty`, or layout-affecting state
  is set. Disable while `animating`.
- Add the box-geometry equivalence test (below).

**Phase 3 — L6 (calc two-pass) as a separate follow-up**, out of scope for the resize
number on the benchmark.

**Explicitly do not do:** L4 (viewport-independence skip) — it cannot help the flex
benchmark and is semantically fragile; L5 (rounding) — negligible and visually risky;
remove the root width bake (L3) without an equivalence test, because the root's own
display mode determines whether available space stretches it (block roots stretch,
flex roots do not necessarily — see `compute/mod.rs:61–115`), so dropping the bake
could change root geometry for non-block roots.

---

## 5. Measurement plan

### 5.1 New/changed bench scenarios

Use quiet-machine runs only — `PERFORMANCE_AUDIT.md:135–153` documents two contaminated
runs that were discarded.

1. **Alternating-width resize** (existing `bench.rs:153–157`, `engine.rs:95–106`):
   primary signal for L2; keep unchanged.
2. **Monotonic new-width resize** (new): `set_viewport(960.0 + (i % 41), 700.0, 1.0)`
   or a continuously increasing width. This defeats Taffy's single-slot final-layout
   cache and any L2 cache; it measures the true worst case and prevents us from
   declaring victory on an artifact of the alternating benchmark. Add to both
   `rendrix-runtime/src/bench.rs` and `engine/crates/rendrix-layout/benches/taffy.rs`
   (`layout_9k_varying_width` already cycles 41 widths — reuse it).
3. **Fixed-layout 9k resize** (new): a 9k tree with only px widths/heights, no
   `flex:1`/`%`. This should already be near-free via Taffy's own cache (§2.1); it
   establishes the floor and validates our understanding of the cache.
4. **Resize-after-mutation** (new): mutate one class, then resize. Validates L2
   invalidation — if the cached geometry is returned after a mutation, the box
   equivalence test must fail, so this is the negative control.

Track columns: total/style/layout/paint/GPU ms plus `shapes`, `restyles`, `layouts`,
and (after Phase 0) Taffy `new_nodes`/`restyled_nodes`.

### 5.2 Correctness / non-regression gates

- `rendrix-runtime/tests/regression.rs:96–113`
  `restyle_equivalence_across_resize_with_static_sheets` — must stay green.
- `regression.rs:120–131` `box_tree_build_is_deterministic` — must stay green.
- `regression.rs:138–158` `allocation_and_shape_budgets`: idle alloc < 256 KB,
  5 hover frames alloc < 1 MB, hover shapes == 0 — must stay green.
- **New box-geometry equivalence test**: build a fresh 9k engine at width W, capture
  all `element_rect` values; build another engine, resize to W through the hot path,
  capture all rects; assert equality for all 9,009 nodes and equality of display-list
  counts (119 items / 1 batch / 773 instances from the audit). This is the definitive
  guard for both L1 and L2.
- Hover and scroll budgets from `PERFORMANCE_AUDIT.md` (< 1.3 ms each) must not regress;
  compare medians from the same quiet run before/after each phase.

### 5.3 Success criteria (do not claim until measured)

- Phase 1: layout ms on the alternating-width resize drops by ≥ the isolated `sync`
  cost measured in Phase 0, with `last_restyled_nodes == 1` on static-sheet resize.
- Phase 2 (if attempted): alternating-width resize layout ms drops substantially,
  monotonic new-width resize is **no slower** than baseline (± variance), and all
  equivalence tests pass.
- No phase may increase hover/scroll/idle medians by more than run-to-run variance,
  increase shape counts, or change the display list.

---

## 6. Assumptions, risks, follow-ups

- **Assumption:** the lockfile's `taffy 0.9.2` is what builds; if integration upgrades
  Taffy, re-verify the cache-key and `mark_dirty` semantics (they differed in older
  and later versions — e.g. 0.15.x has a different tree/cache architecture).
- **Assumption:** `styles` behind `Arc<ComputedStyle>` are never mutated in place. The
  code constructs new `Arc`s on every style change, but this is an invariant L1 depends
  on; document and test it.
- **Risk:** the audit attributes ~8 ms to "Taffy re-solving" but does not isolate
  `sync`/`collect` from `compute_layout_with_measure`. Phase 0 exists specifically to
  avoid optimizing the wrong 8 ms. If `sync` is a small fraction, L1 will not move the
  number and L2 or a Taffy-level change is required.
- **Risk:** L2's correctness surface (revision coverage) is easy to get wrong; prefer
  shipping L1 alone unless the benchmark target explicitly includes repeated widths.
- **Follow-up (out of scope):** the two-pass `calc()` relayout (`taffy_engine.rs:128–147`)
  doubles cost on calc-bearing trees and clones the whole box tree; investigate
  resolving calc dims in a targeted way.
- **Follow-up:** cold-start / large-interface headroom (`PERFORMANCE_AUDIT.md:192–208`)
  remain unmeasured; none of the above addresses 50k+ node scaling, where O(n)
  `sync`/`collect`/`round_layout` walks will dominate regardless.
