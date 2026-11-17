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
    seen: bool,
}

pub struct TaffyEngine {
    tree: TaffyTree<Ctx>,
    nodes: HashMap<u64, Cached>,
    /// Statistics from the last layout, for tests and profiling.
    pub last_new_nodes: usize,
    pub last_restyled_nodes: usize,
    /// Number of nodes that actually went through `sync` (i.e. `to_taffy` was called) during
    /// the last layout. With a dirty hint this is only the changed nodes plus their ancestors;
    /// clean subtrees are skipped. Used by tests to prove sync is dirty-aware.
    pub last_synced_nodes: usize,
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
            last_new_nodes: 0,
            last_restyled_nodes: 0,
            last_synced_nodes: 0,
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

    /// Expand a dirty set to include every ancestor up to the root. A top-down `sync` must not
    /// skip a subtree that contains a dirty node, otherwise a deep mutation would never be
    /// reached (its clean ancestors would short-circuit first). Uses the *box* tree — not the
    /// DOM tree — so generated/anonymous boxes in between are covered too. `|=` is deliberate:
    /// every child must be visited so all affected ancestors get inserted.
    fn mark_affected(n: &LayoutNode, dirty: &HashSet<u64>, affected: &mut HashSet<u64>) -> bool {
        let mut hit = dirty.contains(&n.id);
        for c in &n.children {
            hit |= Self::mark_affected(c, dirty, affected);
        }
        if hit {
            affected.insert(n.id);
        }
        hit
    }

    /// Mark an entire cached subtree as seen without recursing into the live box tree. Used when
    /// a clean subtree is skipped, so stale-node removal does not drop its still-valid Taffy nodes.
    fn mark_seen_subtree(nodes: &mut HashMap<u64, Cached>, root: u64, stack: &mut Vec<u64>) {
        stack.clear();
        stack.push(root);
        while let Some(id) = stack.pop() {
            if let Some(c) = nodes.get_mut(&id) {
                if c.seen {
                    continue;
                }
                c.seen = true;
                stack.extend(c.children.iter().copied());
            }
        }
    }

    fn sync(
        &mut self,
        n: &LayoutNode,
        parent_areas: Option<&Vec<Vec<String>>>,
        stats: &mut (usize, usize),
        tmp: &mut Vec<NodeId>,
        mark: &mut Vec<u64>,
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
            if let Some(c) = self.nodes.get(&n.id) {
                let same_children = c.children.len() == n.children.len()
                    && c.children.iter().zip(n.children.iter()).all(|(a, b)| *a == b.id);
                if same_children && Arc::ptr_eq(&c.source_style, &n.style) && c.kind_sig == Self::kind_signature(&n.kind) {
                    let node = c.node;
                    Self::mark_seen_subtree(&mut self.nodes, n.id, mark);
                    return node;
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
        let (changed_style, changed_kind) = match self.nodes.get(&n.id) {
            Some(c) => (c.style != style, c.kind_sig != kind_sig),
            None => (true, true),
        };
        // A dirty node (or one whose own style/kind actually changed) must re-sync its whole
        // subtree: the patch may have rebuilt descendants, and a child's resolved grid-area
        // placement depends on its parent's template areas. Skipping here would be unsafe, so
        // we fall back to the full path for the subtree instead of skipping clean descendants.
        let force_children = force || dirty.is_some_and(|d| d.contains(&n.id)) || changed_style || changed_kind;

        // Children first (post-order) so ids exist. Child ids accumulate in the scratch
        // buffer (P5: one reused allocation per layout instead of two vecs per node);
        // `set_children`/`new_with_children` only borrow the slice during the call.
        let base = tmp.len();
        // This node's areas are the containing-block areas for its children.
        let my_areas = if n.style.grid_template_areas.is_empty() { None } else { Some(&n.style.grid_template_areas) };
        for c in &n.children {
            let id = self.sync(c, my_areas, stats, tmp, mark, dirty, affected, force_children);
            tmp.push(id);
        }

        if let Some(c) = self.nodes.get_mut(&n.id) {
            c.seen = true;
            if c.style != style {
                self.tree.set_style(c.node, style.clone()).unwrap();
                c.style = style;
                stats.1 += 1;
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
            }
            // P5: compare child identity in place; only allocate the stored vec on change.
            let same_children =
                c.children.len() == n.children.len() && c.children.iter().zip(n.children.iter()).all(|(a, b)| *a == b.id);
            if !same_children {
                self.tree.set_children(c.node, &tmp[base..]).unwrap();
                c.children = n.children.iter().map(|c| c.id).collect();
                stats.1 += 1;
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
                    seen: true,
                },
            );
            tmp.truncate(base);
            stats.0 += 1;
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

        // An empty hint means "no information" (e.g. a viewport-only resize where restyle was
        // skipped). Treat it as no hint so the root is never skipped — skipping the root would
        // prevent us from ever reaching a changed descendant via the Arc backstop below.
        let dirty = dirty.filter(|d| !d.is_empty());
        // Expand dirty box ids to every ancestor so `sync` can always reach a changed node.
        let mut affected = HashSet::new();
        if let Some(d) = dirty {
            Self::mark_affected(root, d, &mut affected);
        }
        // If none of the hinted ids exist in this tree, the hint is unusable — never skip the
        // root in that case, or a real change elsewhere could go unnoticed.
        let dirty = if affected.is_empty() { None } else { dirty };

        for c in self.nodes.values_mut() {
            c.seen = false;
        }
        let mut stats = (0usize, 0usize);
        let mut tmp = Vec::new();
        let mut mark = Vec::new();
        let root_node = self.sync(root, None, &mut stats, &mut tmp, &mut mark, dirty, &affected, false);
        // Drop nodes that disappeared. Skipped subtrees were marked seen above, so they survive.
        let stale: Vec<u64> = self.nodes.iter().filter(|(_, c)| !c.seen).map(|(k, _)| *k).collect();
        for k in stale {
            if let Some(c) = self.nodes.remove(&k) {
                let _ = self.tree.remove(c.node);
            }
        }

        let avail = Size { width: AvailableSpace::Definite(viewport.0), height: AvailableSpace::Definite(viewport.1) };
        compute_with_measure(&mut self.tree, root_node, avail, measurer);
        if dirty.is_some() {
            // Use the ancestor-expanded set: a changed box can move its siblings (flex/grid)
            // and shift its ancestors, so every ancestor must re-collect its whole subtree.
            // Still mutates the caller's map in place, so no fresh ~9000-entry map is allocated.
            refresh_boxes_in_place(&self.tree, root, &self.nodes, Some(&affected), out);
        } else {
            // No hint (bench, calc fallback, viewport-only change): a full collect is the
            // only provably correct answer. Reuse the caller's allocation.
            out.boxes.clear();
            collect(&self.tree, root, &self.nodes, out);
        }

        // Mixed calc(): the first pass approximated Calc as its percent part to obtain
        // containing-block sizes; resolve to px and lay out again for exact geometry.
        if has_calc_in_tree(root) {
            let resolved = resolve_tree_calcs(root, viewport.0, viewport.1, &out.boxes);
            // The resolved tree has fresh style Arcs, so the dirty hint no longer applies.
            for c in self.nodes.values_mut() {
                c.seen = false;
            }
            let mut tmp2 = Vec::new();
            let mut mark2 = Vec::new();
            // The resolved tree has fresh style Arcs and grid contexts on every node, so the
            // dirty hint is meaningless for it: fall back to a forced, full second sync pass.
            let empty = HashSet::new();
            let root_node2 = self.sync(&resolved, None, &mut stats, &mut tmp2, &mut mark2, None, &empty, true);
            let stale2: Vec<u64> = self.nodes.iter().filter(|(_, c)| !c.seen).map(|(k, _)| *k).collect();
            for k in stale2 {
                if let Some(c) = self.nodes.remove(&k) {
                    let _ = self.tree.remove(c.node);
                }
            }
            compute_with_measure(&mut self.tree, root_node2, avail, measurer);
            // Geometry changed tree-wide: always do a full collect for the second pass.
            out.boxes.clear();
            collect(&self.tree, &resolved, &self.nodes, out);
        }

        self.last_new_nodes = stats.0;
        self.last_restyled_nodes = stats.1;
    }
}

impl TaffyEngine {
    /// Like [`LayoutEngine::layout`], but carries forward the geometry of unchanged nodes
    /// from `prev` instead of re-reading Taffy for every box.
    ///
    /// `dirty` names the ids whose geometry may have changed (a dirty node re-collects its
    /// whole subtree). New ids and ids missing from `prev` are always re-collected; ids in
    /// `prev` but absent from `root` drop out. `None` for either arg degrades to a full
    /// collect. Dirty-aware sync still applies: clean subtrees are never re-converted.
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

/// Full-collect wrapper retained for callers that have no previous result.
fn collect(tree: &TaffyTree<Ctx>, n: &LayoutNode, nodes: &HashMap<u64, Cached>, out: &mut LayoutResult) {
    collect_incremental(tree, n, nodes, None, None, out, false);
}

/// Incrementally collect geometry for the tree rooted at `n`.
///
/// `dirty` names ids whose Taffy layout may have changed: a dirty node and all its
/// descendants are re-read from Taffy; every other node carries its `BoxLayout` forward
/// from `prev` when available. New nodes (absent from `prev`) are always re-collected.
/// `None` for `dirty` or `prev` degrades to a full collect.
fn collect_incremental(
    tree: &TaffyTree<Ctx>,
    n: &LayoutNode,
    nodes: &HashMap<u64, Cached>,
    dirty: Option<&HashSet<u64>>,
    prev: Option<&LayoutResult>,
    out: &mut LayoutResult,
    ancestor_dirty: bool,
) {
    let node_dirty = ancestor_dirty || dirty.map_or(true, |d| d.contains(&n.id));
    match prev.filter(|_| !node_dirty).and_then(|p| p.boxes.get(&n.id)) {
        Some(carried) => {
            // Geometry provably unchanged: keep the exact previous BoxLayout.
            out.boxes.insert(n.id, *carried);
        }
        None => {
            let l = tree.layout(nodes[&n.id].node).unwrap();
            out.boxes.insert(
                n.id,
                BoxLayout {
                    x: l.location.x,
                    y: l.location.y,
                    width: l.size.width,
                    height: l.size.height,
                    content_width: l.content_size.width.max(l.size.width - l.border.left - l.border.right),
                    content_height: l.content_size.height.max(l.size.height - l.border.top - l.border.bottom),
                    border: [l.border.top, l.border.right, l.border.bottom, l.border.left],
                    padding: [l.padding.top, l.padding.right, l.padding.bottom, l.padding.left],
                },
            );
        }
    }
    for c in &n.children {
        collect_incremental(tree, c, nodes, dirty, prev, out, node_dirty);
    }
}

/// In-place counterpart: updates `result.boxes` without allocating a new map. `live`
/// accumulates ids present in the current tree so removed nodes can be dropped afterwards.
fn collect_incremental_in_place(
    tree: &TaffyTree<Ctx>,
    n: &LayoutNode,
    nodes: &HashMap<u64, Cached>,
    dirty: Option<&HashSet<u64>>,
    result: &mut LayoutResult,
    live: &mut HashSet<u64>,
    ancestor_dirty: bool,
) {
    let node_dirty = ancestor_dirty || dirty.map_or(true, |d| d.contains(&n.id));
    live.insert(n.id);
    if node_dirty || !result.boxes.contains_key(&n.id) {
        let l = tree.layout(nodes[&n.id].node).unwrap();
        result.boxes.insert(
            n.id,
            BoxLayout {
                x: l.location.x,
                y: l.location.y,
                width: l.size.width,
                height: l.size.height,
                content_width: l.content_size.width.max(l.size.width - l.border.left - l.border.right),
                content_height: l.content_size.height.max(l.size.height - l.border.top - l.border.bottom),
                border: [l.border.top, l.border.right, l.border.bottom, l.border.left],
                padding: [l.padding.top, l.padding.right, l.padding.bottom, l.padding.left],
            },
        );
    }
    for c in &n.children {
        collect_incremental_in_place(tree, c, nodes, dirty, result, live, node_dirty);
    }
}

fn refresh_boxes_in_place(
    tree: &TaffyTree<Ctx>,
    root: &LayoutNode,
    nodes: &HashMap<u64, Cached>,
    dirty: Option<&HashSet<u64>>,
    result: &mut LayoutResult,
) {
    let mut live = HashSet::with_capacity(result.boxes.len());
    collect_incremental_in_place(tree, root, nodes, dirty, result, &mut live, false);
    result.boxes.retain(|id, _| live.contains(id));
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
        e.layout(&root, (800.0, 600.0), &mut FixedText);

        let mut full = LayoutResult::default();
        collect(&e.tree, &root, &e.nodes, &mut full);
        let mut inc = LayoutResult::default();
        collect_incremental(&e.tree, &root, &e.nodes, None, None, &mut inc, false);
        assert_eq!(full.boxes, inc.boxes, "no prev / no dirty must degrade to a full collect");

        let empty_dirty = HashSet::new();
        let mut inc2 = LayoutResult::default();
        collect_incremental(&e.tree, &root, &e.nodes, Some(&empty_dirty), Some(&result_of(&full)), &mut inc2, false);
        assert_eq!(full.boxes, inc2.boxes, "empty dirty set carries forward every clean box");
    }

    #[test]
    fn incremental_collect_reuses_clean_boxes() {
        let root = node(
            1,
            &[("width", "100px")],
            vec![node(2, &[("height", "10px")], vec![]), node(3, &[("height", "10px")], vec![])],
        );
        let mut e = TaffyEngine::new();
        let full = e.layout(&root, (800.0, 600.0), &mut FixedText);

        let mut prev = result_of(&full);
        // A value no Taffy read could produce: if it survives, the BoxLayout was carried
        // forward rather than re-collected (identity proof for Copy data).
        let mut sentinel = prev.boxes[&2];
        sentinel.x = 9876.5;
        prev.boxes.insert(2, sentinel);

        let mut dirty = HashSet::new();
        dirty.insert(3u64);
        let mut inc = LayoutResult::default();
        collect_incremental(&e.tree, &root, &e.nodes, Some(&dirty), Some(&prev), &mut inc, false);

        assert_eq!(inc.boxes[&2].x, 9876.5, "clean node must keep the exact previous BoxLayout");
        assert_eq!(inc.boxes[&3], full.boxes[&3], "dirty node must be re-collected from Taffy");
        assert_eq!(inc.boxes[&1], full.boxes[&1], "non-dirty ancestor is carried forward");
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
        let mut inc = LayoutResult::default();
        collect_incremental(&e.tree, &final_tree, &e.nodes, Some(&dirty), Some(&prev), &mut inc, false);
        assert_eq!(inc.boxes, expected.boxes);
    }

    #[test]
    fn incremental_collect_requires_conservative_dirty_set_for_siblings() {
        // A width change on one flex item moves its siblings' x. The caller must include
        // the flex container in `dirty` (a dirty re-collects its whole subtree), otherwise
        // the carried-forward siblings would keep stale geometry.
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

        let mut dirty = HashSet::new();
        dirty.insert(1u64); // flex container → whole subtree re-collected
        dirty.insert(2u64);
        let mut inc = LayoutResult::default();
        collect_incremental(&e.tree, &final_tree, &e.nodes, Some(&dirty), Some(&prev), &mut inc, false);
        assert_eq!(inc.boxes, expected.boxes);
    }

    #[test]
    fn incremental_collect_matches_full_after_add_remove() {
        let kid = |id, h| node(id, &[("height", h)], vec![]);
        let build = |kids: Vec<LayoutNode>| node(1, &[("width", "100px")], kids);
        let initial = build(vec![kid(2, "10px"), kid(3, "20px")]);
        let final_tree = build(vec![kid(2, "10px"), kid(4, "30px")]); // remove 3, add 4
        let mut e = TaffyEngine::new();
        let first = e.layout(&initial, (800.0, 600.0), &mut FixedText);
        let prev = result_of(&first);
        let expected = e.layout(&final_tree, (800.0, 600.0), &mut FixedText);

        let mut dirty = HashSet::new();
        dirty.insert(1u64); // parent's children changed → parent + descendants re-collected
        dirty.insert(4u64); // brand new node (also re-collected because it is absent from `prev`)
        let mut inc = LayoutResult::default();
        collect_incremental(&e.tree, &final_tree, &e.nodes, Some(&dirty), Some(&prev), &mut inc, false);
        assert_eq!(inc.boxes, expected.boxes);
        assert!(!inc.boxes.contains_key(&3), "removed node must not appear in the result");
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

