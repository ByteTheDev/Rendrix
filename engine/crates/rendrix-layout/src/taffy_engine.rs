//! Taffy-backed [`LayoutEngine`] with a persistent tree.

use crate::{BoxLayout, InlineContent, LayoutEngine, LayoutKind, LayoutNode, LayoutResult, TextMeasurer};
use rendrix_css as css;
use rendrix_css::{Dim, TrackItem, TrackSize};
use std::collections::HashMap;
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
}

impl Default for TaffyEngine {
    fn default() -> Self {
        TaffyEngine::new()
    }
}

impl TaffyEngine {
    pub fn new() -> Self {
        TaffyEngine { tree: TaffyTree::new(), nodes: HashMap::new(), last_new_nodes: 0, last_restyled_nodes: 0 }
    }

    fn sync(&mut self, n: &LayoutNode, parent_areas: Option<&Vec<Vec<String>>>, stats: &mut (usize, usize), tmp: &mut Vec<NodeId>) -> NodeId {
        let style = to_taffy(&n.style, &n.kind, parent_areas);
        let (kind_sig, ctx) = match &n.kind {
            LayoutKind::Box => (0, None),
            LayoutKind::Text(c) => (c.key() | 1, Some(Ctx::Text(c.clone()))),
            LayoutKind::Replaced { width, height } => ((width.to_bits() as u64) << 32 | height.to_bits() as u64 | 2, Some(Ctx::Replaced(*width, *height))),
        };
        // Children first (post-order) so ids exist. Child ids accumulate in the scratch
        // buffer (P5: one reused allocation per layout instead of two vecs per node);
        // `set_children`/`new_with_children` only borrow the slice during the call.
        let base = tmp.len();
        // This node's areas are the containing-block areas for its children.
        let my_areas = if n.style.grid_template_areas.is_empty() { None } else { Some(&n.style.grid_template_areas) };
        for c in &n.children {
            let id = self.sync(c, my_areas, stats, tmp);
            tmp.push(id);
        }

        if let Some(c) = self.nodes.get_mut(&n.id) {
            c.seen = true;
            if c.style != style {
                self.tree.set_style(c.node, style.clone()).unwrap();
                c.style = style;
                stats.1 += 1;
            }
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
                Cached { node, style, kind_sig, children: n.children.iter().map(|c| c.id).collect(), seen: true },
            );
            tmp.truncate(base);
            stats.0 += 1;
            node
        }
    }
}

impl LayoutEngine for TaffyEngine {
    fn layout(&mut self, root: &LayoutNode, viewport: (f32, f32), measurer: &mut dyn TextMeasurer) -> LayoutResult {
        for c in self.nodes.values_mut() {
            c.seen = false;
        }
        let mut stats = (0usize, 0usize);
        let mut tmp = Vec::new();
        let root_node = self.sync(root, None, &mut stats, &mut tmp);
        // Drop nodes that disappeared.
        let stale: Vec<u64> = self.nodes.iter().filter(|(_, c)| !c.seen).map(|(k, _)| *k).collect();
        for k in stale {
            if let Some(c) = self.nodes.remove(&k) {
                let _ = self.tree.remove(c.node);
            }
        }

        let avail = Size { width: AvailableSpace::Definite(viewport.0), height: AvailableSpace::Definite(viewport.1) };
        compute_with_measure(&mut self.tree, root_node, avail, measurer);
        let mut out = LayoutResult::default();
        collect(&self.tree, root, &self.nodes, &mut out);

        // Mixed calc(): the first pass approximated Calc as its percent part to obtain
        // containing-block sizes; resolve to px and lay out again for exact geometry.
        if has_calc_in_tree(root) {
            let resolved = resolve_tree_calcs(root, viewport.0, viewport.1, &out.boxes);
            for c in self.nodes.values_mut() {
                c.seen = false;
            }
            let mut tmp2 = Vec::new();
            let root_node2 = self.sync(&resolved, None, &mut stats, &mut tmp2);
            let stale2: Vec<u64> = self.nodes.iter().filter(|(_, c)| !c.seen).map(|(k, _)| *k).collect();
            for k in stale2 {
                if let Some(c) = self.nodes.remove(&k) {
                    let _ = self.tree.remove(c.node);
                }
            }
            compute_with_measure(&mut self.tree, root_node2, avail, measurer);
            let mut out2 = LayoutResult::default();
            collect(&self.tree, &resolved, &self.nodes, &mut out2);
            out = out2;
        }

        self.last_new_nodes = stats.0;
        self.last_restyled_nodes = stats.1;
        out
    }
}

fn collect(tree: &TaffyTree<Ctx>, n: &LayoutNode, nodes: &HashMap<u64, Cached>, out: &mut LayoutResult) {
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
    for c in &n.children {
        collect(tree, c, nodes, out);
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
}
