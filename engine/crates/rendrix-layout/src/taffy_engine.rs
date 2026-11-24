//! Taffy-backed [`LayoutEngine`] with a persistent tree.

use crate::{BoxLayout, InlineContent, LayoutEngine, LayoutKind, LayoutNode, LayoutResult, TextMeasurer};
use rendrix_css as css;
use rendrix_css::{Dim, TrackItem, TrackSize};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use taffy::prelude::*;
use taffy::geometry::Point;
use taffy::style::{GridTemplateArea, Overflow, GridTemplateComponent, GridTemplateRepetition, MaxTrackSizingFunction, MinTrackSizingFunction, RepetitionCount, TrackSizingFunction};

#[derive(Clone)]
enum Ctx {
    Text(Arc<InlineContent>),
    Replaced(f32, f32),
}

/// `Cached.parent` sentinel: no parent (the root box).
const NO_PARENT: u64 = u64::MAX;

struct Cached {
    node: NodeId,
    style: Style,
    /// The exact source `ComputedStyle` `Arc` that produced `style`. Kept alive so
    /// `Arc::ptr_eq` is a stable, O(1) "this subtree's input is unchanged" test: the restyle
    /// pass retains the old `Arc` whenever computed style is unchanged, and allocates a new
    /// one on any change (see `style::restyle_subtree`). Clean subtree detection uses this
    /// instead of re-converting every node with `to_taffy` each frame.
    source_style: Arc<css::ComputedStyle>,
    kind_sig: u64,
    children: Vec<u64>,
    /// Cached parent box id (`NO_PARENT` for the root). Used to expand a dirty hint to
    /// ancestors in O(dirty x depth) instead of a whole-tree walk, and to resolve moved
    /// vs removed nodes when a parent's child list changes.
    parent: u64,
    /// Epoch of the last layout that visited this node. Compare against `TaffyEngine.epoch`
    /// to test liveness without sweeping every entry each frame.
    seen_epoch: u32,
}

pub struct TaffyEngine {
    tree: TaffyTree<Ctx>,
    nodes: HashMap<u64, Cached>,
    /// Monotonic visit epoch; bumped once per layout.
    epoch: u32,
    /// Box id of the most recent root. A different root id drops the whole cached tree.
    root_id: Option<u64>,
    /// Statistics from the last layout, for tests and profiling.
    pub last_new_nodes: usize,
    pub last_restyled_nodes: usize,
    /// Number of nodes that actually went through `sync` (i.e. `to_taffy` was called) during
    /// the last layout. With a dirty hint this is only the changed nodes plus their ancestors;
    /// clean subtrees are skipped. Used by tests to prove sync is dirty-aware.
    pub last_synced_nodes: usize,
    /// Number of nodes whose geometry was re-read from Taffy during the last layout.
    /// Clean subtrees are carried forward untouched, so a one-item mutation on a large
    /// tree re-reads only the changed chain plus the direct children of re-solved
    /// containers. Used by tests to prove collection is proportional to the change.
    pub last_collected_nodes: usize,
}

impl Default for TaffyEngine {
    fn default() -> Self {
        TaffyEngine::new()
    }
}

impl TaffyEngine {
    pub fn new() -> Self {
        TaffyEngine {
            tree: TaffyTree::new(),
            nodes: HashMap::new(),
            epoch: 0,
            root_id: None,
            last_new_nodes: 0,
            last_restyled_nodes: 0,
            last_synced_nodes: 0,
            last_collected_nodes: 0,
        }
    }

    /// Advance the visit epoch. On wrap, reset every entry once (a full sweep after
    /// ~4 billion layouts, not per frame).
    fn bump_epoch(&mut self) {
        if self.epoch == u32::MAX {
            for c in self.nodes.values_mut() {
                c.seen_epoch = 0;
            }
            self.epoch = 1;
        } else {
            self.epoch += 1;
        }
    }

    /// O(1) signature of a node's [`LayoutKind`]. Text nodes include the memoized
    /// inline-content key, so a text edit always changes the signature and re-syncs that node.
    fn kind_signature(kind: &LayoutKind) -> u64 {
        match kind {
            LayoutKind::Box => 0,
            LayoutKind::Text(c) => c.key() | 1,
            LayoutKind::Replaced { width, height } => (width.to_bits() as u64) << 32 | height.to_bits() as u64 | 2,
        }
    }

    /// Expand a dirty hint to every cached ancestor, in O(dirty x depth) using the cached
    /// parent links instead of walking the whole box tree. A top-down `sync` must not skip a
    /// subtree that contains a dirty node, otherwise a deep mutation would never be reached
    /// (its clean ancestors would short-circuit first). The hint's ids are box-tree ids, so
    /// generated/anonymous boxes in between are covered through the cached chain.
    fn expand_affected(&self, dirty: &HashSet<u64>, affected: &mut HashSet<u64>) {
        for id in dirty {
            let mut cur = *id;
            loop {
                if !affected.insert(cur) {
                    break; // this chain was already expanded through a shared ancestor
                }
                match self.nodes.get(&cur) {
                    Some(c) if c.parent != NO_PARENT => cur = c.parent,
                    _ => break,
                }
            }
        }
    }

    /// Remove a cached subtree: the Taffy nodes, the cache entries and the collected
    /// geometry all go together, so stale geometry can never survive a structural change.
    fn remove_cached_subtree(&mut self, root: u64, out: &mut LayoutResult) {
        let mut stack = vec![root];
        while let Some(id) = stack.pop() {
            if let Some(c) = self.nodes.remove(&id) {
                let _ = self.tree.remove(c.node);
                out.boxes.remove(&id);
                stack.extend(c.children.iter().copied());
            }
        }
    }

    /// Resolve the child ids dropped from re-synced parents this frame. A dropped id that
    /// was visited elsewhere in the same layout is a moved node (kept; its cache entry was
    /// updated by its new parent); one that was never visited is stale and takes its whole
    /// cached subtree with it.
    fn resolve_drops(&mut self, dropped: &[u64], out: &mut LayoutResult) {
        for id in dropped {
            let moved = self.nodes.get(id).is_some_and(|c| c.seen_epoch == self.epoch);
            if !moved {
                self.remove_cached_subtree(*id, out);
            }
        }
    }

    fn sync(
        &mut self,
        n: &LayoutNode,
        parent_id: u64,
        parent_areas: Option<&Vec<Vec<String>>>,
        stats: &mut (usize, usize),
        tmp: &mut Vec<NodeId>,
        dirtied: &mut Vec<(u64, bool)>,
        dropped: &mut Vec<u64>,
        dirty: Option<&HashSet<u64>>,
        affected: &HashSet<u64>,
        force: bool,
    ) -> NodeId {
        // Dirty-aware fast path. A whole subtree can reuse its cached Taffy node when
        //   (a) sync-by-hint is enabled (`dirty` is Some) and no changed node lives in the
        //       subtree (`!affected.contains`) — `affected` includes every ancestor of a dirty
        //       node, so this is exact;
        //   (b) the parent did not force a re-sync (e.g. its grid-template-areas changed);
        //   (c) the child id list, the source style Arc and the kind signature all still match.
        // The Arc check is the correctness backstop: restyle keeps the old Arc for unchanged
        // computed styles and allocates a fresh one on any change, so a stale Arc can never hide
        // a real change even if the dirty hint is incomplete (e.g. `:nth-child` sibling effects).
        if !force && dirty.is_some() && !affected.contains(&n.id) {
            if let Some(c) = self.nodes.get_mut(&n.id) {
                let same_children = c.children.len() == n.children.len()
                    && c.children.iter().zip(n.children.iter()).all(|(a, b)| *a == b.id);
                if same_children && Arc::ptr_eq(&c.source_style, &n.style) && c.kind_sig == Self::kind_signature(&n.kind) {
                    // Mark only this node as visited: its cached child lists are exact (they
                    // matched the live tree), so descendants can only become stale through a
                    // future re-sync of this node, which re-diffs the child list.
                    c.seen_epoch = self.epoch;
                    c.parent = parent_id;
                    return c.node;
                }
            }
        }

        self.last_synced_nodes += 1;
        let style = to_taffy(&n.style, &n.kind, parent_areas);
        let kind_sig = Self::kind_signature(&n.kind);
        let ctx = match &n.kind {
            LayoutKind::Box => None,
            LayoutKind::Text(c) => Some(Ctx::Text(c.clone())),
            LayoutKind::Replaced { width, height } => Some(Ctx::Replaced(*width, *height)),
        };
        // A dirty parent only forces its children through the full path when the parent's
        // grid-template-areas changed: `parent_areas` is the one parent-derived input to a
        // child's `to_taffy` conversion (a named `grid-area` resolves against it), and the
        // fast path compares everything else exactly (children ids, source Arc, kind). A
        // plain child-list or style change needs no forcing — surviving children keep their
        // identities, new nodes have no cache entry and always take the full path, and the
        // parent's re-solve reassigns positions without touching the children's styles.
        let areas_changed = match self.nodes.get(&n.id) {
            Some(c) => c.source_style.grid_template_areas != n.style.grid_template_areas,
            None => true,
        };
        let force_children = force || areas_changed;

        // Children first (post-order) so ids exist. Child ids accumulate in the scratch
        // buffer (P5: one reused allocation per layout instead of two vecs per node);
        // `set_children`/`new_with_children` only borrow the slice during the call.
        let base = tmp.len();
        // This node's areas are the containing-block areas for its children.
        let my_areas = if n.style.grid_template_areas.is_empty() { None } else { Some(&n.style.grid_template_areas) };
        for c in &n.children {
            let id = self.sync(c, n.id, my_areas, stats, tmp, dirtied, dropped, dirty, affected, force_children);
            tmp.push(id);
        }

        if let Some(c) = self.nodes.get_mut(&n.id) {
            c.seen_epoch = self.epoch;
            c.parent = parent_id;
            if c.style != style {
                self.tree.set_style(c.node, style.clone()).unwrap();
                c.style = style;
                stats.1 += 1;
                // A style change clears this node's Taffy cache (and, inside Taffy, every
                // ancestor's): the node re-solves next compute. Its layout inputs changed, so
                // its children's positions may be reassigned even if its own box is identical.
                dirtied.push((n.id, true));
            }
            c.source_style = n.style.clone();
            if c.kind_sig != kind_sig {
                match ctx {
                    Some(ctx) => self.tree.set_node_context(c.node, Some(ctx)).unwrap(),
                    None => self.tree.set_node_context(c.node, None).unwrap(),
                }
                // Changing the context does not dirty the node by itself.
                self.tree.mark_dirty(c.node).unwrap();
                c.kind_sig = kind_sig;
                stats.1 += 1;
                // Re-measure only: the node's layout inputs (style, children) are unchanged,
                // so an identical measured box means the subtree is a deterministic replay.
                dirtied.push((n.id, false));
            }
            // P5: compare child identity in place; only allocate the stored vec on change.
            let same_children =
                c.children.len() == n.children.len() && c.children.iter().zip(n.children.iter()).all(|(a, b)| *a == b.id);
            if !same_children {
                let new_ids: Vec<u64> = n.children.iter().map(|c| c.id).collect();
                // Ids that left this parent's child list are removal candidates; a candidate
                // visited elsewhere this frame is a moved node and survives (resolve_drops).
                for gone in dropped_children(&c.children, &new_ids) {
                    dropped.push(gone);
                }
                self.tree.set_children(c.node, &tmp[base..]).unwrap();
                c.children = new_ids;
                stats.1 += 1;
                // set_children clears this node's cache: its content re-flows next compute,
                // and the child order itself is a layout input for position assignment.
                dirtied.push((n.id, true));
            }
            let node = c.node;
            tmp.truncate(base);
            node
        } else {
            let node = match ctx {
                Some(ctx) => self.tree.new_leaf_with_context(style.clone(), ctx).unwrap(),
                None => self.tree.new_with_children(style.clone(), &tmp[base..]).unwrap(),
            };
            self.nodes.insert(
                n.id,
                Cached {
                    node,
                    style,
                    source_style: n.style.clone(),
                    kind_sig,
                    children: n.children.iter().map(|c| c.id).collect(),
                    parent: parent_id,
                    seen_epoch: self.epoch,
                },
            );
            tmp.truncate(base);
            stats.0 += 1;
            // A fresh Taffy node has no cached layout: it will be solved this compute.
            dirtied.push((n.id, true));
            node
        }
    }

    /// Shared implementation for [`LayoutEngine::layout`] (no dirty hint → full sync) and
    /// [`LayoutEngine::layout_with_dirty`] (skip clean subtrees).
    fn layout_impl(
        &mut self,
        root: &LayoutNode,
        viewport: (f32, f32),
        measurer: &mut dyn TextMeasurer,
        dirty: Option<&HashSet<u64>>,
        out: &mut LayoutResult,
    ) {
        self.last_new_nodes = 0;
        self.last_restyled_nodes = 0;
        self.last_synced_nodes = 0;
        self.last_collected_nodes = 0;
        self.bump_epoch();

        // A different root box means the whole previous tree is unreachable: drop it.
        if let Some(prev_root) = self.root_id {
            if prev_root != root.id {
                self.remove_cached_subtree(prev_root, out);
            }
        }
        self.root_id = Some(root.id);

        // An empty hint means "no information" (e.g. a viewport-only resize where restyle was
        // skipped). Treat it as no hint so the root is never skipped — skipping the root would
        // prevent us from ever reaching a changed descendant via the Arc backstop below.
        let dirty = dirty.filter(|d| !d.is_empty());
        // Expand dirty box ids to every ancestor so `sync` can always reach a changed node.
        let mut affected = HashSet::new();
        if let Some(d) = dirty {
            self.expand_affected(d, &mut affected);
        }
        // If none of the hinted ids are known to this tree, the hint is unusable — never skip
        // the root in that case, or a real change elsewhere could go unnoticed.
        let dirty = if affected.is_empty() { None } else { dirty };

        let mut stats = (0usize, 0usize);
        let mut tmp = Vec::new();
        // (P5: one reused allocation per layout instead of two vecs per node.) `dirtied`
        // accumulates `(box id, inputs_changed)` for every node whose Taffy cache was
        // cleared (style/children/kind change or brand-new node); `dropped` accumulates
        // child ids that left a re-synced parent.
        let mut dirtied: Vec<(u64, bool)> = Vec::new();
        let mut dropped = Vec::new();
        let root_node = self.sync(root, NO_PARENT, None, &mut stats, &mut tmp, &mut dirtied, &mut dropped, dirty, &affected, false);
        // Resolve dropped children against this frame's visits, then their subtrees vanish
        // from the Taffy tree, the cache and the collected geometry together.
        self.resolve_drops(&dropped, out);
        let avail = Size { width: AvailableSpace::Definite(viewport.0), height: AvailableSpace::Definite(viewport.1) };
        compute_with_measure(&mut self.tree, root_node, avail, measurer);
        // Collect only what the sync actually dirtied (plus their ancestors, which Taffy's
        // mark_dirty cleared as well, and the root, which `compute_root_layout` always
        // re-solves). Everything else carries its previous geometry forward untouched.
        self.collect_touched(&dirtied, out);

        // Mixed calc(): the first pass approximated Calc as its percent part to obtain
        // containing-block sizes; resolve to px and lay out again for exact geometry.
        if has_calc_in_tree(root) {
            let resolved = resolve_tree_calcs(root, viewport.0, viewport.1, &out.boxes);
            // The resolved tree has fresh style Arcs, so the dirty hint no longer applies.
            let mut dirtied2: Vec<(u64, bool)> = Vec::new();
            let mut dropped2 = Vec::new();
            // The resolved tree has fresh style Arcs and grid contexts on every node, so the
            // dirty hint is meaningless for it: fall back to a forced, full second sync pass.
            let empty = HashSet::new();
            let root_node2 = self.sync(&resolved, NO_PARENT, None, &mut stats, &mut tmp, &mut dirtied2, &mut dropped2, None, &empty, true);
            self.resolve_drops(&dropped2, out);
            compute_with_measure(&mut self.tree, root_node2, avail, measurer);
            // Geometry changed tree-wide: always do a full collect for the second pass.
            out.boxes.clear();
            let mut all = 0usize;
            collect_full(&self.tree, &resolved, &self.nodes, out, &mut all);
            self.last_collected_nodes = all;
        }

        self.last_new_nodes = stats.0;
        self.last_restyled_nodes = stats.1;
    }

    /// Incremental collect: re-read geometry only for the nodes Taffy re-solved this frame.
    ///
    /// `dirtied` carries `(id, inputs_changed)` pairs from the sync: `inputs_changed` marks a
    /// style/children/new-node change — such a node can reassign its children's positions even
    /// when its own box geometry is identical (`justify-content` moving siblings), so it always
    /// counts as changed. A re-measured node (new text) counts as changed only when its
    /// measured box actually differs. The changed set expands to cached ancestors (their
    /// Taffy caches were cleared too) plus the root, which `compute_root_layout` re-solves
    /// unconditionally.
    ///
    /// Walking descends with a compare-and-stop rule: a child whose geometry is byte-identical
    /// to the previous frame cannot have re-solved descendants (its re-solve inputs — available
    /// space and known dimensions — are derived from its own unchanged box), so the subtree is
    /// carried forward as-is; the direct children of a must node are always re-read once
    /// because a re-solve can reassign their positions even when their sizes do not change.
    /// Ids missing from `out` are always re-read, so a caller with an empty result degrades
    /// to a full collect.
    fn collect_touched(&mut self, dirtied: &[(u64, bool)], out: &mut LayoutResult) {
        let mut must: HashSet<u64> = HashSet::with_capacity(dirtied.len().saturating_mul(2) + 1);
        if let Some(root) = self.root_id {
            must.insert(root);
        }
        let mut visited: HashSet<u64> = HashSet::with_capacity(must.len().saturating_mul(4) + 8);
        let mut collected = 0usize;
        // Read every dirtied node first (they were re-solved, so their caches are gone).
        for &(id, inputs_changed) in dirtied {
            let differs = self.collect_read(id, &mut visited, out, &mut collected);
            if inputs_changed || differs {
                must.insert(id);
                let mut cur = id;
                while let Some(c) = self.nodes.get(&cur) {
                    let p = c.parent;
                    if p == NO_PARENT || !must.insert(p) {
                        break;
                    }
                    cur = p;
                }
            }
        }
        // One node can be reachable from several `must` roots (a chain node is both an
        // ancestor of a changed node and a walk root itself); read each box at most once.
        for id in &must {
            self.collect_node(*id, &must, &mut visited, out, &mut collected);
        }
        self.last_collected_nodes = collected;
    }

    /// Read one node's fresh geometry into `out`. Returns whether it differs from the
    /// previously carried value (a missing entry counts as changed: it must be re-read).
    fn collect_read(&self, id: u64, visited: &mut HashSet<u64>, out: &mut LayoutResult, collected: &mut usize) -> bool {
        let Some(c) = self.nodes.get(&id) else { return false };
        if !visited.insert(id) {
            // Already read this frame (a node can be dirtied twice, e.g. style + children).
            return false;
        }
        let l = self.tree.layout(c.node).unwrap();
        let b = box_layout_of(l);
        *collected += 1;
        let differs = out.boxes.get(&id).is_none_or(|prev| *prev != b);
        out.boxes.insert(id, b);
        differs
    }

    fn collect_node(
        &self,
        id: u64,
        must: &HashSet<u64>,
        visited: &mut HashSet<u64>,
        out: &mut LayoutResult,
        collected: &mut usize,
    ) {
        let Some(c) = self.nodes.get(&id) else { return };
        if !visited.insert(id) {
            // Read once per frame. A `must` node that the pre-pass already read still has
            // to descend: its re-solve may have reassigned its children's positions.
            if must.contains(&id) {
                for child in &c.children {
                    self.collect_node(*child, must, visited, out, collected);
                }
            }
            return;
        }
        let l = self.tree.layout(c.node).unwrap();
        let b = box_layout_of(l);
        *collected += 1;
        let carried_identical = out.boxes.get(&id).is_some_and(|prev| *prev == b);
        out.boxes.insert(id, b);
        if carried_identical && !must.contains(&id) {
            // Geometry identical and the node never re-solved: its subtree cannot have moved.
            return;
        }
        for child in &c.children {
            self.collect_node(*child, must, visited, out, collected);
        }
    }
}

/// Copy a Taffy [`Layout`] into the engine-neutral [`BoxLayout`].
fn box_layout_of(l: &taffy::tree::Layout) -> BoxLayout {
    BoxLayout {
        x: l.location.x,
        y: l.location.y,
        width: l.size.width,
        height: l.size.height,
        content_width: l.content_size.width.max(l.size.width - l.border.left - l.border.right),
        content_height: l.content_size.height.max(l.size.height - l.border.top - l.border.bottom),
        border: [l.border.top, l.border.right, l.border.bottom, l.border.left],
        padding: [l.padding.top, l.padding.right, l.padding.bottom, l.padding.left],
    }
}

/// Ids present in `old` but missing from `new` (children that left this parent). Common
/// prefixes/suffixes are trimmed first, so appending or removing at either end costs no
/// membership tests; the middle remainder uses a scratch set when it is not tiny.
fn dropped_children(old: &[u64], new: &[u64]) -> Vec<u64> {
    let mut p = 0;
    while p < old.len() && p < new.len() && old[p] == new[p] {
        p += 1;
    }
    let mut s = 0;
    while s < old.len() - p && s < new.len() - p && old[old.len() - 1 - s] == new[new.len() - 1 - s] {
        s += 1;
    }
    let old_mid = &old[p..old.len() - s];
    if old_mid.is_empty() {
        return Vec::new();
    }
    let new_mid = &new[p..new.len() - s];
    if new_mid.len() > 16 {
        let set: HashSet<u64> = new_mid.iter().copied().collect();
        old_mid.iter().copied().filter(|id| !set.contains(id)).collect()
    } else {
        old_mid.iter().copied().filter(|id| !new_mid.contains(id)).collect()
    }
}

impl TaffyEngine {
    /// Like [`LayoutEngine::layout`], but carries forward the geometry of unchanged nodes
    /// from `prev` instead of re-reading Taffy for every box.
    ///
    /// `dirty` names the ids whose boxes changed (style/kind/children); the engine syncs
    /// only those subtrees plus their ancestors, and re-collects only what Taffy actually
    /// re-solved — the changed chain, its ancestors and the direct children of re-solved
    /// containers. New ids and ids missing from `prev` are always re-collected; ids that
    /// left the tree drop out. `None` for either arg degrades to a full sync/collect.
    pub fn layout_incremental(
        &mut self,
        root: &LayoutNode,
        viewport: (f32, f32),
        measurer: &mut dyn TextMeasurer,
        prev: Option<&LayoutResult>,
        dirty: Option<&HashSet<u64>>,
    ) -> LayoutResult {
        let mut out = match prev {
            Some(p) => LayoutResult { boxes: p.boxes.clone() },
            None => LayoutResult::default(),
        };
        self.layout_impl(root, viewport, measurer, dirty, &mut out);
        out
    }

    /// Allocation-saving variant used by the engine: rebuilds `result.boxes` in place so a
    /// clean frame does not allocate a fresh ~9000-entry `HashMap`.
    pub fn layout_incremental_in_place(
        &mut self,
        root: &LayoutNode,
        viewport: (f32, f32),
        measurer: &mut dyn TextMeasurer,
        result: &mut LayoutResult,
        dirty: Option<&HashSet<u64>>,
    ) {
        self.layout_impl(root, viewport, measurer, dirty, result);
    }
}

impl LayoutEngine for TaffyEngine {
    fn layout(&mut self, root: &LayoutNode, viewport: (f32, f32), measurer: &mut dyn TextMeasurer) -> LayoutResult {
        // No dirty hint (benches, direct callers, calc fallback): sync the full tree so behavior
        // is byte-for-byte identical to the pre-dirty-aware implementation.
        let mut out = LayoutResult::default();
        self.layout_impl(root, viewport, measurer, None, &mut out);
        out
    }

    fn layout_with_dirty(
        &mut self,
        root: &LayoutNode,
        viewport: (f32, f32),
        measurer: &mut dyn TextMeasurer,
        dirty: &HashSet<u64>,
    ) -> LayoutResult {
        let mut out = LayoutResult::default();
        self.layout_impl(root, viewport, measurer, Some(dirty), &mut out);
        out
    }
}

/// Full recursive collect for paths with no incremental information (the mixed-calc second
/// pass, whose geometry changes tree-wide). Every reachable box is re-read from Taffy.
fn collect_full(tree: &TaffyTree<Ctx>, n: &LayoutNode, nodes: &HashMap<u64, Cached>, out: &mut LayoutResult, collected: &mut usize) {
    if let Some(c) = nodes.get(&n.id) {
        let l = tree.layout(c.node).unwrap();
        *collected += 1;
        out.boxes.insert(n.id, box_layout_of(l));
    }
    for c in &n.children {
        collect_full(tree, c, nodes, out, collected);
    }
}

fn compute_with_measure(tree: &mut TaffyTree<Ctx>, root_node: NodeId, avail: Size<AvailableSpace>, measurer: &mut dyn TextMeasurer) {
    tree.compute_layout_with_measure(root_node, avail, |known, available, _id, ctx, _style| match ctx {
        None => Size::ZERO,
        Some(Ctx::Replaced(w, h)) => {
            let (iw, ih) = (*w, *h);
            match (known.width, known.height) {
                (Some(w), Some(h)) => Size { width: w, height: h },
                (Some(w), None) => Size { width: w, height: if iw > 0.0 { w * ih / iw } else { ih } },
                (None, Some(h)) => Size { width: if ih > 0.0 { h * iw / ih } else { iw }, height: h },
                (None, None) => Size { width: iw, height: ih },
            }
        }
        Some(Ctx::Text(content)) => {
            let max_w = match known.width {
                Some(w) => Some(w),
                None => match available.width {
                    AvailableSpace::Definite(w) => Some(w),
                    AvailableSpace::MinContent => Some(0.0),
                    AvailableSpace::MaxContent => None,
                },
            };
            let (w, h) = measurer.measure(content, max_w);
            Size { width: known.width.unwrap_or(w), height: known.height.unwrap_or(h) }
        }
    })
    .unwrap();
}

// ---------------------------------------------------------------------------------------------
// Mixed calc() resolution (second layout pass)
// ---------------------------------------------------------------------------------------------

fn is_layout_calc(d: Dim) -> bool {
    matches!(d, Dim::Calc(..))
}

fn has_calc_in_style(s: &css::ComputedStyle) -> bool {
    let any = |ds: &[Dim]| ds.iter().any(|d| is_layout_calc(*d));
    any(&[s.width, s.height, s.min_width, s.min_height, s.max_width, s.max_height, s.flex_basis, s.row_gap, s.column_gap])
        || any(&s.margin)
        || any(&s.padding)
        || any(&s.inset)
}

fn has_calc_in_tree(n: &LayoutNode) -> bool {
    if has_calc_in_style(&n.style) {
        return true;
    }
    n.children.iter().any(has_calc_in_tree)
}

fn resolve_calc_dim(d: Dim, basis: f32) -> Dim {
    match d {
        Dim::Calc(p, px) => Dim::Px(basis * p / 100.0 + px),
        d => d,
    }
}

/// Replace layout-relevant `Calc` dims with px resolved against the containing block.
/// Horizontal dims resolve against `parent_w`, vertical against `parent_h`;
/// margin/padding resolve against the width per CSS.
fn resolve_style_calcs(s: &css::ComputedStyle, parent_w: f32, parent_h: f32) -> css::ComputedStyle {
    let mut out = s.clone();
    out.width = resolve_calc_dim(s.width, parent_w);
    out.min_width = resolve_calc_dim(s.min_width, parent_w);
    out.max_width = resolve_calc_dim(s.max_width, parent_w);
    out.height = resolve_calc_dim(s.height, parent_h);
    out.min_height = resolve_calc_dim(s.min_height, parent_h);
    out.max_height = resolve_calc_dim(s.max_height, parent_h);
    out.flex_basis = resolve_calc_dim(s.flex_basis, parent_w);
    for i in 0..4 {
        out.margin[i] = resolve_calc_dim(s.margin[i], parent_w);
        out.padding[i] = resolve_calc_dim(s.padding[i], parent_w);
    }
    out.inset[0] = resolve_calc_dim(s.inset[0], parent_h);
    out.inset[2] = resolve_calc_dim(s.inset[2], parent_h);
    out.inset[1] = resolve_calc_dim(s.inset[1], parent_w);
    out.inset[3] = resolve_calc_dim(s.inset[3], parent_w);
    out.row_gap = resolve_calc_dim(s.row_gap, parent_h);
    out.column_gap = resolve_calc_dim(s.column_gap, parent_w);
    out
}

fn content_box_size(b: &BoxLayout) -> (f32, f32) {
    let w = (b.width - b.border[1] - b.border[3] - b.padding[1] - b.padding[3]).max(0.0);
    let h = (b.height - b.border[0] - b.border[2] - b.padding[0] - b.padding[2]).max(0.0);
    (w, h)
}

fn resolve_tree_calcs(n: &LayoutNode, parent_w: f32, parent_h: f32, boxes: &HashMap<u64, BoxLayout>) -> LayoutNode {
    let style = std::sync::Arc::new(resolve_style_calcs(&n.style, parent_w, parent_h));
    let (my_w, my_h) = boxes.get(&n.id).map(content_box_size).unwrap_or((parent_w, parent_h));
    let children = n.children.iter().map(|c| resolve_tree_calcs(c, my_w, my_h, boxes)).collect();
    LayoutNode { id: n.id, style, kind: n.kind.clone(), children }
}

// ---------------------------------------------------------------------------------------------
// grid-template-areas
// ---------------------------------------------------------------------------------------------

/// CSS line numbers (1-indexed, end exclusive) of a named area.
fn area_bounds(areas: &[Vec<String>], name: &str) -> Option<(i16, i16, i16, i16)> {
    let mut rows = (usize::MAX, 0usize);
    let mut cols = (usize::MAX, 0usize);
    let mut found = false;
    for (r, row) in areas.iter().enumerate() {
        for (c, n) in row.iter().enumerate() {
            if n == name {
                found = true;
                rows.0 = rows.0.min(r);
                rows.1 = rows.1.max(r);
                cols.0 = cols.0.min(c);
                cols.1 = cols.1.max(c);
            }
        }
    }
    if !found {
        return None;
    }
    Some((rows.0 as i16 + 1, rows.1 as i16 + 2, cols.0 as i16 + 1, cols.1 as i16 + 2))
}

fn grid_areas_for_taffy(areas: &[Vec<String>]) -> Vec<GridTemplateArea<String>> {
    use std::collections::BTreeMap;
    let mut bounds: BTreeMap<&str, (u16, u16, u16, u16)> = BTreeMap::new();
    for (r, row) in areas.iter().enumerate() {
        for (c, name) in row.iter().enumerate() {
            if name == "." {
                continue;
            }
            bounds
                .entry(name.as_str())
                .and_modify(|b| {
                    b.0 = b.0.min(r as u16);
                    b.1 = b.1.max(r as u16 + 1);
                    b.2 = b.2.min(c as u16);
                    b.3 = b.3.max(c as u16 + 1);
                })
                .or_insert((r as u16, r as u16 + 1, c as u16, c as u16 + 1));
        }
    }
    bounds.into_iter().map(|(name, (rs, re, cs, ce))| GridTemplateArea { name: name.to_string(), row_start: rs, row_end: re, column_start: cs, column_end: ce }).collect()
}

// ---------------------------------------------------------------------------------------------
// CSS → Taffy
// ---------------------------------------------------------------------------------------------

fn dim(d: Dim) -> Dimension {
    match d {
        Dim::Auto => Dimension::auto(),
        Dim::Px(v) => Dimension::length(v),
        Dim::Percent(p) => Dimension::percent(p / 100.0),
        // Mixed calc(): first-pass approximation uses the percent part; a second layout
        // pass resolves it to px against the containing-block size (see `layout`).
        Dim::Calc(p, _) => Dimension::percent(p / 100.0),
    }
}

fn lpa(d: Dim) -> LengthPercentageAuto {
    match d {
        Dim::Auto => LengthPercentageAuto::auto(),
        Dim::Px(v) => LengthPercentageAuto::length(v),
        Dim::Percent(p) => LengthPercentageAuto::percent(p / 100.0),
        Dim::Calc(p, _) => LengthPercentageAuto::percent(p / 100.0),
    }
}

fn lp(d: Dim) -> LengthPercentage {
    match d {
        Dim::Auto => LengthPercentage::length(0.0),
        Dim::Px(v) => LengthPercentage::length(v),
        Dim::Percent(p) => LengthPercentage::percent(p / 100.0),
        Dim::Calc(p, _) => LengthPercentage::percent(p / 100.0),
    }
}

fn align_items(a: css::Align) -> Option<AlignItems> {
    use css::Align::*;
    Some(match a {
        Start => AlignItems::Start,
        End => AlignItems::End,
        FlexStart => AlignItems::FlexStart,
        FlexEnd => AlignItems::FlexEnd,
        Center => AlignItems::Center,
        Stretch => AlignItems::Stretch,
        Baseline => AlignItems::Baseline,
        Auto | Normal | SpaceBetween | SpaceAround | SpaceEvenly => return None,
    })
}

fn align_content(a: css::Align) -> Option<AlignContent> {
    use css::Align::*;
    Some(match a {
        Start => AlignContent::Start,
        End => AlignContent::End,
        FlexStart => AlignContent::FlexStart,
        FlexEnd => AlignContent::FlexEnd,
        Center => AlignContent::Center,
        Stretch => AlignContent::Stretch,
        SpaceBetween => AlignContent::SpaceBetween,
        SpaceAround => AlignContent::SpaceAround,
        SpaceEvenly => AlignContent::SpaceEvenly,
        Auto | Normal | Baseline => return None,
    })
}

fn overflow(o: css::Overflow) -> Overflow {
    match o {
        css::Overflow::Visible => Overflow::Visible,
        css::Overflow::Hidden => Overflow::Hidden,
        css::Overflow::Clip => Overflow::Clip,
        css::Overflow::Scroll | css::Overflow::Auto => Overflow::Scroll,
    }
}

fn track_min(t: &TrackSize) -> MinTrackSizingFunction {
    match t {
        TrackSize::Px(v) => MinTrackSizingFunction::length(*v),
        TrackSize::Percent(p) => MinTrackSizingFunction::percent(p / 100.0),
        TrackSize::MinContent => MinTrackSizingFunction::min_content(),
        TrackSize::MaxContent => MinTrackSizingFunction::max_content(),
        TrackSize::MinMax(a, _) => track_min(a),
        TrackSize::Auto | TrackSize::Fr(_) | TrackSize::FitContent(_) => MinTrackSizingFunction::auto(),
    }
}

fn track_max(t: &TrackSize) -> MaxTrackSizingFunction {
    match t {
        TrackSize::Px(v) => MaxTrackSizingFunction::length(*v),
        TrackSize::Percent(p) => MaxTrackSizingFunction::percent(p / 100.0),
        TrackSize::MinContent => MaxTrackSizingFunction::min_content(),
        TrackSize::MaxContent => MaxTrackSizingFunction::max_content(),
        TrackSize::Fr(f) => MaxTrackSizingFunction::fr(*f),
        TrackSize::FitContent(px) => MaxTrackSizingFunction::fit_content_px(*px),
        TrackSize::MinMax(_, b) => track_max(b),
        TrackSize::Auto => MaxTrackSizingFunction::auto(),
    }
}

fn track(t: &TrackSize) -> TrackSizingFunction {
    TrackSizingFunction { min: track_min(t), max: track_max(t) }
}

fn template(items: &[TrackItem]) -> Vec<GridTemplateComponent<String>> {
    items
        .iter()
        .map(|i| match i {
            TrackItem::Single(t) => GridTemplateComponent::Single(track(t)),
            TrackItem::Repeat(count, tracks) => GridTemplateComponent::Repeat(GridTemplateRepetition {
                count: match count {
                    css::RepeatCount::Count(n) => RepetitionCount::Count(*n),
                    css::RepeatCount::AutoFill => RepetitionCount::AutoFill,
                    css::RepeatCount::AutoFit => RepetitionCount::AutoFit,
                },
                tracks: tracks.iter().map(track).collect(),
                line_names: Vec::new(),
            }),
        })
        .collect()
}

fn placement(l: css::GridLine) -> GridPlacement<String> {
    match l {
        css::GridLine::Auto => GridPlacement::Auto,
        css::GridLine::Line(n) => taffy::style_helpers::line(n),
        css::GridLine::Span(n) => taffy::style_helpers::span(n),
    }
}

pub(crate) fn to_taffy(s: &css::ComputedStyle, kind: &LayoutKind, parent_areas: Option<&Vec<Vec<String>>>) -> Style {
    use css::Display as D;
    let display = match s.display {
        D::None => Display::None,
        D::Flex | D::InlineFlex => Display::Flex,
        D::Grid | D::InlineGrid => Display::Grid,
        D::Block | D::Inline | D::InlineBlock | D::Contents => Display::Block,
        // Tables collapse to a grid-like layout (boxtree normalises the container and
        // dissolves rows/groups); columns generate no box. Lists keep a block + marker run.
        D::Table | D::InlineTable => Display::Grid,
        D::TableColumn | D::TableColumnGroup => Display::None,
        D::TableCaption | D::TableHeaderGroup | D::TableRowGroup | D::TableFooterGroup | D::TableRow | D::TableCell | D::ListItem => Display::Block,
    };
    let mut st = Style {
        display,
        box_sizing: match s.box_sizing {
            css::BoxSizing::BorderBox => BoxSizing::BorderBox,
            css::BoxSizing::ContentBox => BoxSizing::ContentBox,
        },
        overflow: Point { x: overflow(s.overflow_x), y: overflow(s.overflow_y) },
        scrollbar_width: 0.0,
        position: match s.position {
            css::Position::Absolute | css::Position::Fixed => Position::Absolute,
            _ => Position::Relative,
        },
        // `position: sticky` lays out at its static position: the insets are thresholds
        // applied as a paint-time clamp in the nearest scrollport (see `sticky_rect`),
        // not relative offsets. Zeroing them here keeps Taffy from shifting the box.
        inset: if matches!(s.position, css::Position::Sticky) {
            Rect { top: lpa(Dim::Auto), right: lpa(Dim::Auto), bottom: lpa(Dim::Auto), left: lpa(Dim::Auto) }
        } else {
            Rect { top: lpa(s.inset[0]), right: lpa(s.inset[1]), bottom: lpa(s.inset[2]), left: lpa(s.inset[3]) }
        },
        size: Size { width: dim(s.width), height: dim(s.height) },
        min_size: Size { width: dim(s.min_width), height: dim(s.min_height) },
        max_size: Size { width: dim(s.max_width), height: dim(s.max_height) },
        aspect_ratio: s.aspect_ratio,
        margin: Rect { top: lpa(s.margin[0]), right: lpa(s.margin[1]), bottom: lpa(s.margin[2]), left: lpa(s.margin[3]) },
        padding: Rect { top: lp(s.padding[0]), right: lp(s.padding[1]), bottom: lp(s.padding[2]), left: lp(s.padding[3]) },
        border: Rect {
            top: LengthPercentage::length(s.border_width[0]),
            right: LengthPercentage::length(s.border_width[1]),
            bottom: LengthPercentage::length(s.border_width[2]),
            left: LengthPercentage::length(s.border_width[3]),
        },
        align_items: align_items(s.align_items),
        align_self: align_items(s.align_self),
        justify_items: align_items(s.justify_items),
        justify_self: align_items(s.justify_self),
        align_content: align_content(s.align_content),
        justify_content: align_content(s.justify_content),
        gap: Size { width: lp(s.column_gap), height: lp(s.row_gap) },
        flex_direction: match s.flex_direction {
            css::FlexDirection::Row => FlexDirection::Row,
            css::FlexDirection::RowReverse => FlexDirection::RowReverse,
            css::FlexDirection::Column => FlexDirection::Column,
            css::FlexDirection::ColumnReverse => FlexDirection::ColumnReverse,
        },
        flex_wrap: match s.flex_wrap {
            css::FlexWrap::NoWrap => FlexWrap::NoWrap,
            css::FlexWrap::Wrap => FlexWrap::Wrap,
            css::FlexWrap::WrapReverse => FlexWrap::WrapReverse,
        },
        flex_basis: dim(s.flex_basis),
        flex_grow: s.flex_grow,
        flex_shrink: s.flex_shrink,
        ..Style::default()
    };
    if matches!(display, Display::Grid) {
        st.grid_template_columns = template(&s.grid_template_columns);
        st.grid_template_rows = template(&s.grid_template_rows);
        st.grid_template_areas = grid_areas_for_taffy(&s.grid_template_areas);
        st.grid_auto_columns = s.grid_auto_columns.iter().map(track).collect();
        st.grid_auto_rows = s.grid_auto_rows.iter().map(track).collect();
        st.grid_auto_flow = match s.grid_auto_flow {
            css::GridAutoFlow::Row => GridAutoFlow::Row,
            css::GridAutoFlow::Column => GridAutoFlow::Column,
            css::GridAutoFlow::RowDense => GridAutoFlow::RowDense,
            css::GridAutoFlow::ColumnDense => GridAutoFlow::ColumnDense,
        };
    }
    // Grid placement applies to *items*, regardless of this node's own display.
    // `grid-area: <name>` resolves against the parent's template areas to explicit lines.
    if let (Some(name), Some(areas)) = (s.grid_area.as_ref(), parent_areas) {
        if let Some((rs, re, cs, ce)) = area_bounds(areas, name) {
            st.grid_row = Line { start: placement(css::GridLine::Line(rs)), end: placement(css::GridLine::Line(re)) };
            st.grid_column = Line { start: placement(css::GridLine::Line(cs)), end: placement(css::GridLine::Line(ce)) };
        } else {
            st.grid_column = Line { start: placement(s.grid_column.0), end: placement(s.grid_column.1) };
            st.grid_row = Line { start: placement(s.grid_row.0), end: placement(s.grid_row.1) };
        }
    } else {
        st.grid_column = Line { start: placement(s.grid_column.0), end: placement(s.grid_column.1) };
        st.grid_row = Line { start: placement(s.grid_row.0), end: placement(s.grid_row.1) };
    }
    st.item_is_replaced = matches!(kind, LayoutKind::Replaced { .. });
    st
}

#[cfg(test)]
mod tests {
    use super::*;
    use rendrix_css::{apply_declaration, ComputedStyle, StyleContext};

    struct FixedText;
    impl TextMeasurer for FixedText {
        /// 10px per character, 20px per line.
        fn measure(&mut self, c: &InlineContent, max_width: Option<f32>) -> (f32, f32) {
            let text: String = c.runs.iter().map(|r| r.text.as_str()).collect();
            let w = text.chars().count() as f32 * 10.0;
            match max_width {
                Some(m) if m > 0.0 && w > m => {
                    let per_line = (m / 10.0).floor().max(1.0);
                    let lines = (text.chars().count() as f32 / per_line).ceil();
                    (per_line * 10.0, lines * 20.0)
                }
                _ => (w, 20.0),
            }
        }
    }

    fn style(decls: &[(&str, &str)]) -> Arc<ComputedStyle> {
        let parent = ComputedStyle::default();
        let mut s = ComputedStyle::inherit_from(&parent);
        s.display = css::Display::Block;
        let sc = StyleContext::default();
        for (k, v) in decls {
            apply_declaration(&mut s, &parent, k, v, &sc);
        }
        Arc::new(s)
    }

    fn node(id: u64, decls: &[(&str, &str)], children: Vec<LayoutNode>) -> LayoutNode {
        LayoutNode { id, style: style(decls), kind: LayoutKind::Box, children }
    }

    fn text(id: u64, decls: &[(&str, &str)], s: &str) -> LayoutNode {
        let st = style(decls);
        let content = Arc::new(InlineContent::new(
            vec![crate::InlineRun { text: s.into(), style: st.clone(), node: id, is_break: false }],
            st.clone(),
        ));
        LayoutNode { id, style: st, kind: LayoutKind::Text(content), children: vec![] }
    }

    #[test]
    fn flex_row_distributes_grow_and_gap() {
        let root = node(
            1,
            &[("display", "flex"), ("width", "300px"), ("height", "100px"), ("gap", "10px")],
            vec![node(2, &[("flex", "1")], vec![]), node(3, &[("width", "100px")], vec![]), node(4, &[("flex", "2")], vec![])],
        );
        let mut e = TaffyEngine::new();
        let r = e.layout(&root, (800.0, 600.0), &mut FixedText);
        // 300 - 100 (fixed) - 2 gaps (20) = 180 free → 60 / 120.
        assert_eq!(r.boxes[&2].width, 60.0);
        assert_eq!(r.boxes[&3].width, 100.0);
        assert_eq!(r.boxes[&4].width, 120.0);
        assert_eq!(r.boxes[&3].x, 70.0);
        assert_eq!(r.boxes[&2].height, 100.0, "align-items: normal stretches");
    }

    #[test]
    fn centering_with_justify_and_align() {
        let root = node(
            1,
            &[("display", "flex"), ("width", "200px"), ("height", "100px"), ("justify-content", "center"), ("align-items", "center")],
            vec![node(2, &[("width", "50px"), ("height", "20px")], vec![])],
        );
        let r = TaffyEngine::new().layout(&root, (800.0, 600.0), &mut FixedText);
        assert_eq!((r.boxes[&2].x, r.boxes[&2].y), (75.0, 40.0));
    }

    #[test]
    fn block_flow_padding_border_and_percent() {
        let root = node(
            1,
            &[("width", "400px"), ("padding", "10px"), ("border", "2px solid red")],
            vec![node(2, &[("height", "30px"), ("width", "50%")], vec![]), node(3, &[("height", "40px"), ("margin-top", "5px")], vec![])],
        );
        let r = TaffyEngine::new().layout(&root, (800.0, 600.0), &mut FixedText);
        assert_eq!(r.boxes[&1].width, 424.0, "content-box: 400 + 2*10 + 2*2");
        assert_eq!(r.boxes[&2].width, 200.0);
        assert_eq!(r.boxes[&2].x, 12.0);
        assert_eq!(r.boxes[&3].y, 12.0 + 30.0 + 5.0);
        assert_eq!(r.boxes[&1].height, 99.0, "30 + 5 + 40 content, plus 2*10 padding and 2*2 border");
    }

    #[test]
    fn text_wraps_to_available_width_and_shrinks_to_fit() {
        let root = node(1, &[("width", "100px")], vec![text(2, &[], "abcdefghijklmnopqrstu")]);
        let r = TaffyEngine::new().layout(&root, (800.0, 600.0), &mut FixedText);
        assert_eq!(r.boxes[&2].width, 100.0);
        assert_eq!(r.boxes[&2].height, 60.0, "21 chars at 10 per line → 3 lines");
        let row = node(1, &[("display", "flex")], vec![text(2, &[("padding", "5px")], "hello")]);
        let r = TaffyEngine::new().layout(&row, (800.0, 600.0), &mut FixedText);
        assert_eq!(r.boxes[&2].width, 60.0, "flex item shrink-wraps its text plus padding");
        assert_eq!(r.boxes[&2].height, 30.0);
    }

    #[test]
    fn absolute_positioning_and_overflow_extent() {
        let root = node(
            1,
            &[("position", "relative"), ("width", "200px"), ("height", "100px"), ("overflow", "hidden")],
            vec![
                node(2, &[("position", "absolute"), ("right", "10px"), ("bottom", "5px"), ("width", "30px"), ("height", "20px")], vec![]),
                node(3, &[("height", "300px")], vec![]),
            ],
        );
        let r = TaffyEngine::new().layout(&root, (800.0, 600.0), &mut FixedText);
        assert_eq!((r.boxes[&2].x, r.boxes[&2].y), (160.0, 75.0));
        assert_eq!(r.boxes[&1].height, 100.0);
        assert!(r.boxes[&1].content_height >= 300.0, "scroll extent covers overflowing child: {}", r.boxes[&1].content_height);
    }

    #[test]
    fn grid_template_columns() {
        let kids = (2..=5).map(|i| node(i, &[("height", "10px")], vec![])).collect();
        let root = node(1, &[("display", "grid"), ("width", "300px"), ("grid-template-columns", "100px 1fr 1fr"), ("gap", "10px")], kids);
        let r = TaffyEngine::new().layout(&root, (800.0, 600.0), &mut FixedText);
        assert_eq!(r.boxes[&2].width, 100.0);
        assert_eq!(r.boxes[&3].width, 90.0);
        assert_eq!(r.boxes[&4].width, 90.0);
        assert_eq!(r.boxes[&5].x, 0.0, "fourth item wraps to the next row");
        assert_eq!(r.boxes[&5].y, 20.0);
    }

    #[test]
    fn unchanged_tree_restyles_nothing_and_edits_touch_only_changes() {
        let build = |w: &str| node(1, &[("width", "100px")], vec![node(2, &[("width", w)], vec![]), node(3, &[("height", "5px")], vec![])]);
        let mut e = TaffyEngine::new();
        e.layout(&build("10px"), (800.0, 600.0), &mut FixedText);
        assert_eq!(e.last_new_nodes, 3);
        e.layout(&build("10px"), (800.0, 600.0), &mut FixedText);
        assert_eq!((e.last_new_nodes, e.last_restyled_nodes), (0, 0));
        let r = e.layout(&build("20px"), (800.0, 600.0), &mut FixedText);
        assert_eq!((e.last_new_nodes, e.last_restyled_nodes), (0, 1));
        assert_eq!(r.boxes[&2].width, 20.0);
    }

    #[test]
    fn display_none_and_removed_nodes() {
        let mut e = TaffyEngine::new();
        let a = node(1, &[("width", "100px")], vec![node(2, &[("height", "10px")], vec![]), node(3, &[("height", "10px")], vec![])]);
        let r = e.layout(&a, (800.0, 600.0), &mut FixedText);
        assert_eq!(r.boxes[&1].height, 20.0);
        let b = node(1, &[("width", "100px")], vec![node(2, &[("height", "10px"), ("display", "none")], vec![]), node(3, &[("height", "10px")], vec![])]);
        let r = e.layout(&b, (800.0, 600.0), &mut FixedText);
        assert_eq!(r.boxes[&1].height, 10.0);
        let c = node(1, &[("width", "100px")], vec![node(3, &[("height", "10px")], vec![])]);
        e.layout(&c, (800.0, 600.0), &mut FixedText);
        assert_eq!(e.nodes.len(), 2, "removed nodes are dropped from the persistent tree");
    }

    #[test]
    fn calc_mixed_width_resolves_against_parent() {
        // `calc(100% - 20px)` inside a 400px parent leaves 380px.
        let root = node(
            1,
            &[("width", "400px")],
            vec![node(2, &[("width", "calc(100% - 20px)"), ("height", "10px")], vec![])],
        );
        let r = TaffyEngine::new().layout(&root, (800.0, 600.0), &mut FixedText);
        assert!((r.boxes[&2].width - 380.0).abs() < 0.6, "mixed calc width: {:?}", r.boxes[&2]);
        assert!((r.boxes[&2].x - 0.0).abs() < 0.6);
    }

    #[test]
    fn grid_template_areas_place_items() {
        let root = node(
            1,
            &[
                ("display", "grid"),
                ("width", "300px"),
                ("grid-template-columns", "100px 1fr"),
                ("grid-template-rows", "50px 50px"),
                ("grid-template-areas", "\"header header\" \"sidebar main\""),
            ],
            vec![
                node(2, &[("grid-area", "header"), ("height", "10px")], vec![]),
                node(3, &[("grid-area", "sidebar"), ("height", "10px")], vec![]),
                node(4, &[("grid-area", "main"), ("height", "10px")], vec![]),
            ],
        );
        let r = TaffyEngine::new().layout(&root, (800.0, 600.0), &mut FixedText);
        let (h, s, m) = (&r.boxes[&2], &r.boxes[&3], &r.boxes[&4]);
        assert!((h.x - 0.0).abs() < 0.6 && (h.width - 300.0).abs() < 0.6, "header spans both columns: {h:?}");
        assert!((h.y - 0.0).abs() < 0.6, "header in the first row: {h:?}");
        assert!((s.x - 0.0).abs() < 0.6 && (s.width - 100.0).abs() < 0.6, "sidebar in the first column: {s:?}");
        assert!((s.y - 50.0).abs() < 0.6, "sidebar in the second row: {s:?}");
        assert!((m.x - 100.0).abs() < 0.6 && (m.width - 200.0).abs() < 0.6, "main in the second column: {m:?}");
        assert!((m.y - 50.0).abs() < 0.6, "main in the second row: {m:?}");
    }

    #[test]
    fn table_display_maps_to_grid_and_keeps_explicit_tracks() {
        // What boxtree generates for a table: Grid display, so cells share the row.
        let kids = (2..=5).map(|i| node(i, &[("height", "10px")], vec![])).collect();
        let root = node(1, &[("display", "table"), ("width", "300px"), ("grid-template-columns", "100px 1fr")], kids);
        let r = TaffyEngine::new().layout(&root, (800.0, 600.0), &mut FixedText);
        assert_eq!(r.boxes[&2].width, 100.0);
        assert_eq!(r.boxes[&3].x, 100.0);
        assert_eq!(r.boxes[&4].y, 10.0, "third cell wraps to the next row");
    }

    #[test]
    fn table_display_mapping_covers_all_internal_boxes() {
        let mut table = ComputedStyle::inherit_from(&ComputedStyle::default());
        table.display = css::Display::Table;
        assert_eq!(to_taffy(&table, &LayoutKind::Box, None).display, Display::Grid);
        let mut col = ComputedStyle::inherit_from(&ComputedStyle::default());
        col.display = css::Display::TableColumn;
        assert_eq!(to_taffy(&col, &LayoutKind::Box, None).display, Display::None, "columns generate no box");
        let mut cell = ComputedStyle::inherit_from(&ComputedStyle::default());
        cell.display = css::Display::TableCell;
        assert_eq!(to_taffy(&cell, &LayoutKind::Box, None).display, Display::Block, "cells are grid items");
    }

    // -----------------------------------------------------------------------------------------
    // Dirty-aware sync (the #1 residual cost called out in PERFORMANCE_AUDIT.md)
    // -----------------------------------------------------------------------------------------

    #[test]
    fn dirty_aware_sync_skips_clean_subtrees() {
        // Reuse the same Arc for every unchanged node, exactly as the real restyle pass does
        // (`style.rs` keeps the old Arc when computed style is unchanged). Only node 3 gets a
        // fresh Arc, so the cached-Arc check can tell it apart from the clean siblings.
        let s1 = style(&[("display", "flex")]);
        let s2 = style(&[("width", "10px")]);
        let s4 = style(&[("width", "30px")]);
        let build = |s3: Arc<ComputedStyle>| LayoutNode {
            id: 1,
            style: s1.clone(),
            kind: LayoutKind::Box,
            children: vec![
                LayoutNode { id: 2, style: s2.clone(), kind: LayoutKind::Box, children: vec![] },
                LayoutNode { id: 3, style: s3, kind: LayoutKind::Box, children: vec![] },
                LayoutNode { id: 4, style: s4.clone(), kind: LayoutKind::Box, children: vec![] },
            ],
        };
        let mut e = TaffyEngine::new();
        e.layout(&build(style(&[("width", "10px")])), (800.0, 600.0), &mut FixedText);
        assert_eq!(e.last_synced_nodes, 4, "first layout has no cache and syncs every node");

        let mut dirty = HashSet::new();
        dirty.insert(3u64);
        let r = e.layout_with_dirty(&build(style(&[("width", "20px")])), (800.0, 600.0), &mut FixedText, &dirty);
        // The changed node (3) and its ancestor (1, needed to reach it) are re-synced; clean
        // siblings 2 and 4 (and their subtrees) are skipped without calling `to_taffy`.
        assert_eq!(e.last_synced_nodes, 2, "clean siblings must not be re-synced");
        assert_eq!((e.last_new_nodes, e.last_restyled_nodes), (0, 1));
        assert_eq!(r.boxes[&3].width, 20.0);
        assert_eq!(e.nodes.len(), 4, "skipped subtrees survive stale-node removal");
    }

    #[test]
    fn dirty_aware_sync_recalcs_children_when_parent_grid_areas_change() {
        // A child's resolved grid placement depends on its parent's grid-template-areas. Only
        // the parent is dirty here, so the child must be force-re-synced even though its own
        // style Arc is unchanged — otherwise its placement would be stale.
        let child = LayoutNode {
            id: 2,
            style: style(&[("grid-area", "a"), ("height", "10px")]),
            kind: LayoutKind::Box,
            children: vec![],
        };
        let build = |areas: &str| LayoutNode {
            id: 1,
            style: style(&[
                ("display", "grid"),
                ("width", "300px"),
                ("grid-template-columns", "100px 200px"),
                ("grid-template-rows", "50px"),
                ("grid-template-areas", areas),
            ]),
            kind: LayoutKind::Box,
            children: vec![child.clone()],
        };
        let mut e = TaffyEngine::new();
        let r = e.layout(&build("\"a b\""), (800.0, 600.0), &mut FixedText);
        assert!((r.boxes[&2].x - 0.0).abs() < 0.6, "area 'a' starts in column 0: {:?}", r.boxes[&2]);

        let mut dirty = HashSet::new();
        dirty.insert(1u64); // only the grid container changed
        let r = e.layout_with_dirty(&build("\"b a\""), (800.0, 600.0), &mut FixedText, &dirty);
        assert!((r.boxes[&2].x - 100.0).abs() < 0.6, "area 'a' moved to column 1: {:?}", r.boxes[&2]);
    }

    #[test]
    fn empty_dirty_hint_falls_back_to_full_sync() {
        // An empty hint carries no information, so nothing may be skipped — the root included.
        // Both the root and the child are reused Arcs; only the child's style changes, and we
        // deliberately pass no dirty ids to prove the engine cannot skip the root and miss it.
        let s1 = style(&[("width", "100px")]);
        let build = |s2: Arc<ComputedStyle>| LayoutNode {
            id: 1,
            style: s1.clone(),
            kind: LayoutKind::Box,
            children: vec![LayoutNode { id: 2, style: s2, kind: LayoutKind::Box, children: vec![] }],
        };
        let mut e = TaffyEngine::new();
        e.layout(&build(style(&[("width", "10px")])), (800.0, 600.0), &mut FixedText);
        assert_eq!(e.last_synced_nodes, 2);
        let dirty = HashSet::new();
        let r = e.layout_with_dirty(&build(style(&[("width", "20px")])), (800.0, 600.0), &mut FixedText, &dirty);
        assert_eq!(e.last_synced_nodes, 2, "empty hint must not skip the root");
        assert_eq!(r.boxes[&2].width, 20.0);
    }
    // -----------------------------------------------------------------------------------------
    // Incremental collection
    // -----------------------------------------------------------------------------------------

    fn result_of(r: &LayoutResult) -> LayoutResult {
        LayoutResult { boxes: r.boxes.clone() }
    }

    #[test]
    fn incremental_collect_matches_full_collect() {
        let root = node(
            1,
            &[("width", "300px")],
            vec![node(2, &[("height", "10px")], vec![]), node(3, &[("height", "10px")], vec![]), node(4, &[("height", "10px")], vec![])],
        );
        let mut e = TaffyEngine::new();
        let full = e.layout(&root, (800.0, 600.0), &mut FixedText);

        // No change and an empty dirty hint: every box carries forward, values identical.
        let empty = HashSet::new();
        let inc = e.layout_incremental(&root, (800.0, 600.0), &mut FixedText, Some(&full), Some(&empty));
        assert_eq!(inc.boxes, full.boxes);
        // Only the root (re-solved unconditionally) plus its direct children were re-read.
        assert_eq!(e.last_collected_nodes, 4, "root + 3 direct children re-read, nothing deeper");
    }

    #[test]
    fn incremental_collect_carries_clean_subtrees_forward_untouched() {
        // A value no Taffy read could produce: if it survives, the BoxLayout was carried
        // forward rather than re-collected (identity proof for Copy data). The sentinel sits
        // DEEP inside a clean sibling subtree — nodes under a re-solved parent are always
        // re-read (their positions can be reassigned), but a clean subtree is never entered.
        let root = node(
            1,
            &[("width", "100px")],
            vec![node(2, &[("height", "10px")], vec![node(4, &[("height", "10px")], vec![])]), node(3, &[("height", "10px")], vec![])],
        );
        let mut e = TaffyEngine::new();
        let full = e.layout(&root, (800.0, 600.0), &mut FixedText);

        let mut prev = result_of(&full);
        let mut sentinel = prev.boxes[&4];
        sentinel.x = 9876.5;
        prev.boxes.insert(4, sentinel);

        let mut dirty = HashSet::new();
        dirty.insert(3u64);
        let inc = e.layout_incremental(&root, (800.0, 600.0), &mut FixedText, Some(&prev), Some(&dirty));

        assert_eq!(inc.boxes[&4].x, 9876.5, "clean subtree must be carried forward untouched");
        assert_eq!(inc.boxes[&3], full.boxes[&3], "dirty node must be re-collected from Taffy");
        assert_eq!(inc.boxes[&1], full.boxes[&1], "the re-solved root is re-collected");
        assert_eq!(inc.boxes[&2], full.boxes[&2], "direct children of a re-solved node are re-read");
    }

    #[test]
    fn incremental_collect_matches_full_after_style_change() {
        let build = |w: &str| {
            node(
                1,
                &[("width", "100px")],
                vec![node(2, &[("height", "10px")], vec![]), node(3, &[("height", "10px"), ("width", w)], vec![])],
            )
        };
        let initial = build("10px");
        let final_tree = build("20px");
        let mut e = TaffyEngine::new();
        let first = e.layout(&initial, (800.0, 600.0), &mut FixedText);
        let prev = result_of(&first);
        let expected = e.layout(&final_tree, (800.0, 600.0), &mut FixedText);

        let mut dirty = HashSet::new();
        dirty.insert(3u64);
        let inc = e.layout_incremental(&final_tree, (800.0, 600.0), &mut FixedText, Some(&prev), Some(&dirty));
        assert_eq!(inc.boxes, expected.boxes);
    }

    #[test]
    fn incremental_collect_re_reads_moved_siblings_without_caller_hint() {
        // A width change on one flex item moves its siblings' x even though the fixed-size
        // container's own geometry is identical. The ancestor of a re-solved node always
        // descends one level, so the shifted sibling is re-read without the caller having
        // to name the container in the hint.
        let build = |w: &str| {
            node(
                1,
                &[("display", "flex"), ("width", "300px"), ("height", "100px")],
                vec![node(2, &[("width", w)], vec![]), node(3, &[("width", "50px")], vec![])],
            )
        };
        let initial = build("50px");
        let final_tree = build("100px");
        let mut e = TaffyEngine::new();
        let first = e.layout(&initial, (800.0, 600.0), &mut FixedText);
        let prev = result_of(&first);
        let expected = e.layout(&final_tree, (800.0, 600.0), &mut FixedText);
        assert_ne!(expected.boxes[&3].x, first.boxes[&3].x, "clean sibling x really moves");
        assert_eq!(expected.boxes[&1], first.boxes[&1], "fixed-size container geometry is identical");

        let mut dirty = HashSet::new();
        dirty.insert(2u64); // only the changed item — not the container
        let inc = e.layout_incremental(&final_tree, (800.0, 600.0), &mut FixedText, Some(&prev), Some(&dirty));
        assert_eq!(inc.boxes, expected.boxes, "shifted sibling must be re-read through the ancestor chain");
    }

    #[test]
    fn incremental_collect_matches_full_after_add_remove() {
        let kid = |id, h| node(id, &[("height", h)], vec![]);
        let build = |kids: Vec<LayoutNode>| node(1, &[("width", "100px")], kids);
        let initial = build(vec![kid(2, "10px"), kid(3, "20px")]);
        let final_tree = build(vec![kid(2, "10px"), kid(4, "30px")]); // remove 3, add 4

        // Reference engine: the expected geometry comes from a full layout of the new tree.
        let mut reference = TaffyEngine::new();
        reference.layout(&initial, (800.0, 600.0), &mut FixedText);
        let expected = reference.layout(&final_tree, (800.0, 600.0), &mut FixedText);

        let mut e = TaffyEngine::new();
        let first = e.layout(&initial, (800.0, 600.0), &mut FixedText);
        let prev = result_of(&first);

        let mut dirty = HashSet::new();
        dirty.insert(1u64); // the parent's child list changed
        let inc = e.layout_incremental(&final_tree, (800.0, 600.0), &mut FixedText, Some(&prev), Some(&dirty));
        assert_eq!(inc.boxes, expected.boxes);
        assert!(!inc.boxes.contains_key(&3), "removed node must not appear in the result");
        assert!(!e.nodes.contains_key(&3), "removed node must leave the persistent tree");
    }

    #[test]
    fn collection_is_proportional_to_the_change() {
        // 11 containers x 6 text leaves = 78 boxes. One leaf's text changes; the collector
        // must touch the changed chain plus the direct children of the root (which is always
        // re-solved), never the leaves under the 10 clean containers. Unchanged nodes keep
        // their style Arcs (clone), exactly as the real restyle/patch pipeline guarantees.
        let build = || {
            let items: Vec<LayoutNode> = (0..11)
                .map(|i| {
                    let leaves: Vec<LayoutNode> =
                        (0..6).map(|k| text(200 + i * 10 + k, &[], &format!("item {i} leaf {k}"))).collect();
                    node(20 + i, &[("height", "10px")], leaves)
                })
                .collect();
            node(1, &[("width", "100px")], items)
        };
        let edit_leaf = |mut tree: LayoutNode| {
            for it in tree.children.iter_mut() {
                for c in it.children.iter_mut() {
                    if c.id == 205 {
                        let st = c.style.clone();
                        let content = Arc::new(InlineContent::new(
                            vec![crate::InlineRun { text: "edited leaf text".into(), style: st.clone(), node: 205, is_break: false }],
                            st,
                        ));
                        c.kind = LayoutKind::Text(content);
                    }
                }
            }
            tree
        };
        let mut e = TaffyEngine::new();
        let base = build();
        let first = e.layout(&base, (800.0, 600.0), &mut FixedText);
        assert_eq!(e.nodes.len(), 1 + 11 + 66);
        assert_eq!(e.last_collected_nodes, 78, "a cold layout collects the whole tree");

        let mut dirty = HashSet::new();
        dirty.insert(205u64); // leaf 5 of container 20
        let mut carried = result_of(&first);
        e.layout_incremental_in_place(&edit_leaf(base.clone()), (800.0, 600.0), &mut FixedText, &mut carried, Some(&dirty));
        // Root (always re-solved) + its 11 direct children + container 20's 6 leaves, with a
        // couple of duplicate walk roots from the small `must` set. The 60 leaves under the
        // 10 clean containers are never entered.
        assert!(e.last_collected_nodes <= 22, "one text edit must not re-read clean subtrees: {}", e.last_collected_nodes);
        assert!(e.last_synced_nodes <= 4, "one text edit must not re-sync the whole tree: {}", e.last_synced_nodes);
        // Geometry: the incremental result must equal a full re-layout of the edited tree.
        let mut reference = TaffyEngine::new();
        reference.layout(&base, (800.0, 600.0), &mut FixedText);
        let expected = reference.layout(&edit_leaf(base), (800.0, 600.0), &mut FixedText);
        assert_eq!(carried.boxes, expected.boxes);
        for i in 0..11 {
            assert_eq!(carried.boxes[&(20 + i)].height, 10.0);
        }
    }

    #[test]
    fn moved_node_survives_child_list_diff_with_one_cache_entry() {
        // Move child 3 from container 2 to container 4. The child-list diff must report 3 as
        // dropped from container 2, the visit at its new location must keep it alive, and the
        // cache must hold exactly one entry for it (not a stale duplicate).
        let build = |order: u32| {
            let three = node(3, &[("height", "10px")], vec![]);
            let (a, b) = if order == 0 {
                (node(2, &[("height", "10px")], vec![three]), node(4, &[("height", "10px")], vec![]))
            } else {
                (node(2, &[("height", "10px")], vec![]), node(4, &[("height", "10px")], vec![three]))
            };
            node(1, &[("width", "100px")], vec![a, b])
        };
        let mut e = TaffyEngine::new();
        e.layout(&build(0), (800.0, 600.0), &mut FixedText);
        assert_eq!(e.nodes.len(), 4);

        let mut dirty = HashSet::new();
        dirty.insert(2u64);
        dirty.insert(4u64);
        let r = e.layout_with_dirty(&build(1), (800.0, 600.0), &mut FixedText, &dirty);
        assert_eq!(e.nodes.len(), 4, "the moved node keeps its single cache entry");
        assert_eq!(e.nodes[&3].parent, 4, "the moved node's cached parent follows it");
        // Blocks stack: container 4 (height 10) starts below container 2, and the moved
        // node is laid out as container 4's first child.
        assert_eq!((r.boxes[&2].y, r.boxes[&2].height), (0.0, 10.0), "container 2 shrank back to its own height");
        assert_eq!(r.boxes[&4].y, 10.0, "container 4 sits below container 2");
        assert_eq!(r.boxes[&3].y, 0.0, "moved node is laid out at the top of its new parent");
        assert_eq!(r.boxes[&1].height, 20.0, "root holds both containers");
    }

    #[test]
    fn removed_subtree_leaves_no_stale_geometry_or_cache() {
        let inner = node(3, &[("height", "10px")], vec![node(4, &[("height", "10px")], vec![])]);
        let build = |with_inner: bool| {
            node(
                1,
                &[("width", "100px")],
                if with_inner { vec![node(2, &[("height", "10px")], vec![]), inner.clone()] } else { vec![node(2, &[("height", "10px")], vec![])] },
            )
        };
        let mut e = TaffyEngine::new();
        let first = e.layout(&build(true), (800.0, 600.0), &mut FixedText);
        assert!(first.boxes.contains_key(&4));

        let mut dirty = HashSet::new();
        dirty.insert(1u64);
        let mut carried = result_of(&first);
        e.layout_incremental_in_place(&build(false), (800.0, 600.0), &mut FixedText, &mut carried, Some(&dirty));
        assert!(!carried.boxes.contains_key(&3), "removed geometry must drop out of the carried result");
        assert!(!carried.boxes.contains_key(&4), "removed subtree geometry must drop out too");
        assert!(!e.nodes.contains_key(&3) && !e.nodes.contains_key(&4), "removed subtree leaves the cache");
    }

    #[test]
    fn replacing_the_root_drops_the_previous_tree() {
        let mut e = TaffyEngine::new();
        e.layout(&node(1, &[("width", "100px")], vec![node(2, &[("height", "10px")], vec![])]), (800.0, 600.0), &mut FixedText);
        assert_eq!(e.nodes.len(), 2);
        let r = e.layout(&node(9, &[("width", "100px")], vec![node(10, &[("height", "10px")], vec![])]), (800.0, 600.0), &mut FixedText);
        assert_eq!(e.nodes.len(), 2, "the old root's subtree must not linger in the cache");
        assert!(r.boxes.contains_key(&10));
    }

    #[test]
    fn style_change_with_identical_geometry_still_re_reads_children() {
        // `justify-content` moves the child but changes neither the container's size nor
        // position. A collector that stops at "own geometry identical" would keep the
        // child's stale x; the node's Taffy inputs changed, so its children must be re-read.
        let build = |justify: &str| {
            node(
                1,
                &[("display", "flex"), ("width", "300px"), ("height", "100px"), ("justify-content", justify)],
                vec![node(2, &[("width", "50px"), ("height", "10px")], vec![])],
            )
        };
        let mut e = TaffyEngine::new();
        let first = e.layout(&build("flex-start"), (800.0, 600.0), &mut FixedText);
        assert_eq!(first.boxes[&2].x, 0.0);

        let mut dirty = HashSet::new();
        dirty.insert(1u64);
        let prev = result_of(&first);
        let inc = e.layout_incremental(&build("center"), (800.0, 600.0), &mut FixedText, Some(&prev), Some(&dirty));
        assert_eq!(inc.boxes[&1], first.boxes[&1], "container geometry is identical");
        assert_eq!(inc.boxes[&2].x, 125.0, "child moved to the center and must be re-read");
    }

    #[test]
    fn text_change_with_identical_box_geometry_touches_nothing_else() {
        // The same text height and width: the leaf's measured box is identical, so the
        // re-measure is a deterministic replay — no ancestor or sibling geometry can move.
        let build = |t: &str| {
            node(
                1,
                &[("width", "300px")],
                vec![
                    node(2, &[("height", "20px")], vec![]),
                    node(3, &[("height", "20px")], vec![text(4, &[], t), text(5, &[], "untouched")]),
                ],
            )
        };
        let mut e = TaffyEngine::new();
        let first = e.layout(&build("aaaa"), (800.0, 600.0), &mut FixedText);

        // Same measurement: 4 chars -> 40px wide, one line -> identical box geometry.
        let mut dirty = HashSet::new();
        dirty.insert(4u64);
        let mut prev = result_of(&first);
        // A sentinel on the edited leaf's *sibling* proves the parent's other children were
        // carried forward untouched (an identical re-measure cannot move them).
        let mut sentinel = prev.boxes[&5];
        sentinel.x = 9876.5;
        prev.boxes.insert(5, sentinel);
        e.layout_incremental_in_place(&build("bbbb"), (800.0, 600.0), &mut FixedText, &mut prev, Some(&dirty));
        assert_eq!(prev.boxes[&5].x, 9876.5, "sibling of an identical-measure text swap is untouched");
        assert_eq!(prev.boxes[&4], first.boxes[&4], "identical-measure text swap keeps the leaf's box");
        // The walk read the leaf, the root and the root's direct children: nothing deeper.
        assert!(e.last_collected_nodes <= 4, "identical-measure text swap must not re-read the tree: {}", e.last_collected_nodes);
    }

    #[test]
    fn dropped_children_reports_only_missing_ids() {
        assert!(dropped_children(&[1, 2, 3], &[1, 2, 3]).is_empty(), "identical lists drop nothing");
        assert!(dropped_children(&[1, 2, 3], &[1, 2, 3, 4, 5]).is_empty(), "appending drops nothing");
        assert_eq!(dropped_children(&[1, 2, 3, 4], &[1, 2]), vec![3, 4], "tail removal via suffix trim");
        assert_eq!(dropped_children(&[1, 2, 3], &[3]), vec![1, 2], "head removal via prefix trim");
        assert_eq!(dropped_children(&[1, 2, 3, 4], &[1, 4, 2]), vec![3], "middle removal with a move");
        assert!(dropped_children(&[1, 2, 3], &[3, 1, 2]).is_empty(), "pure reorder drops nothing");
        assert_eq!(dropped_children(&[1, 2, 3], &[]), vec![1, 2, 3], "clearing children drops all");
    }

    #[test]
    fn layout_incremental_matches_layout_with_and_without_prev() {
        let root = node(
            1,
            &[("width", "300px"), ("height", "100px")],
            vec![node(2, &[("height", "20px")], vec![]), node(3, &[("height", "20px")], vec![])],
        );
        let full = TaffyEngine::new().layout(&root, (800.0, 600.0), &mut FixedText);

        let inc = TaffyEngine::new().layout_incremental(&root, (800.0, 600.0), &mut FixedText, None, None);
        assert_eq!(inc.boxes, full.boxes, "no prev / no dirty must match the regular layout path");

        let mut e = TaffyEngine::new();
        let warm = e.layout(&root, (800.0, 600.0), &mut FixedText);
        let prev = result_of(&warm);
        let empty_dirty = HashSet::new();
        let inc2 = e.layout_incremental(&root, (800.0, 600.0), &mut FixedText, Some(&prev), Some(&empty_dirty));
        assert_eq!(inc2.boxes, full.boxes);
    }

    #[test]
    fn layout_incremental_matches_layout_after_mutation() {
        let build = |w| {
            node(
                1,
                &[("width", "100px")],
                vec![node(2, &[("height", "10px")], vec![]), node(3, &[("height", "10px"), ("width", w)], vec![])],
            )
        };
        let initial = build("10px");
        let final_tree = build("20px");

        let mut reference = TaffyEngine::new();
        reference.layout(&initial, (800.0, 600.0), &mut FixedText);
        let expected = reference.layout(&final_tree, (800.0, 600.0), &mut FixedText);

        let mut e = TaffyEngine::new();
        let warm = e.layout(&initial, (800.0, 600.0), &mut FixedText);
        let prev = result_of(&warm);
        let mut dirty = HashSet::new();
        dirty.insert(3u64);
        let got = e.layout_incremental(&final_tree, (800.0, 600.0), &mut FixedText, Some(&prev), Some(&dirty));
        assert_eq!(got.boxes, expected.boxes);
    }

    #[test]
    fn layout_incremental_in_place_matches_layout() {
        let build = |w| {
            node(
                1,
                &[("width", "100px")],
                vec![node(2, &[("height", "10px")], vec![]), node(3, &[("height", "10px"), ("width", w)], vec![])],
            )
        };
        let initial = build("10px");
        let final_tree = build("20px");

        let mut reference = TaffyEngine::new();
        reference.layout(&initial, (800.0, 600.0), &mut FixedText);
        let expected = reference.layout(&final_tree, (800.0, 600.0), &mut FixedText);

        let mut e = TaffyEngine::new();
        let mut result = e.layout(&initial, (800.0, 600.0), &mut FixedText);
        let mut dirty = HashSet::new();
        dirty.insert(3u64);
        e.layout_incremental_in_place(&final_tree, (800.0, 600.0), &mut FixedText, &mut result, Some(&dirty));
        assert_eq!(result.boxes, expected.boxes);

        // A clean second pass leaves the map untouched (and does not allocate a new one).
        let snapshot = result.boxes.clone();
        let empty = HashSet::new();
        e.layout_incremental_in_place(&final_tree, (800.0, 600.0), &mut FixedText, &mut result, Some(&empty));
        assert_eq!(result.boxes, snapshot);

        // Nodes removed from the tree are dropped from the reused map.
        let shorter = node(1, &[("width", "100px")], vec![node(2, &[("height", "10px")], vec![])]);
        let mut dirty2 = HashSet::new();
        dirty2.insert(1u64);
        e.layout_incremental_in_place(&shorter, (800.0, 600.0), &mut FixedText, &mut result, Some(&dirty2));
        assert!(!result.boxes.contains_key(&3));
    }

    #[test]
    fn layout_incremental_matches_layout_with_calc() {
        let root = node(
            1,
            &[("width", "400px")],
            vec![node(2, &[("width", "calc(100% - 20px)"), ("height", "10px")], vec![])],
        );
        let full = TaffyEngine::new().layout(&root, (800.0, 600.0), &mut FixedText);

        let mut e = TaffyEngine::new();
        let warm = e.layout(&root, (800.0, 600.0), &mut FixedText);
        let prev = result_of(&warm);
        let empty_dirty = HashSet::new();
        let inc = e.layout_incremental(&root, (800.0, 600.0), &mut FixedText, Some(&prev), Some(&empty_dirty));
        assert_eq!(inc.boxes, full.boxes, "calc trees re-collect fully in the second pass");
    }
}

