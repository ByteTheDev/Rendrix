//! Box generation: DOM + computed styles → the layout-engine-neutral box tree.
//!
//! This is where CSS visual formatting model rules live: block vs inline vs flex/grid containers,
//! anonymous boxes, inline formatting contexts (runs of text sharing one paragraph), whitespace
//! collapsing, replaced elements, form controls, generated content and containing blocks for
//! absolutely positioned boxes.

use crate::Engine;
use rendrix_css::{Align, ComputedStyle, Dim, Display, TextAlign, WhiteSpace};
use rendrix_dom::{NodeId, NodeKind};
use rendrix_layout::{InlineContent, InlineRun, LayoutKind, LayoutNode};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

pub const ANON: u64 = 1 << 40;

pub(crate) fn anon_id(parent: u64, idx: u64) -> u64 {
    ANON | (parent << 8) | (idx & 0xff)
}

enum Item {
    Run(InlineRun),
    Atomic(LayoutNode),
    Block(LayoutNode),
}

pub(crate) struct Builder<'a> {
    pub engine: &'a Engine,
    /// Collection points for absolutely positioned descendants, innermost positioned ancestor last.
    abs_stack: Vec<Vec<LayoutNode>>,
    /// P1: fragments of the previous frame's box tree available for reuse, keyed by box id.
    /// Anonymous leaves are never pooled (they always rebuild cheaply from runs).
    taken: HashMap<u64, LayoutNode>,
    /// P1: DOM ids whose box subtree must be rebuilt (this frame's style/state roots).
    dirty: HashSet<NodeId>,
}

fn is_text_input(engine: &Engine, id: NodeId) -> bool {
    engine.doc.is_text_control(id)
}

impl<'a> Builder<'a> {
    pub fn new(engine: &'a Engine) -> Self {
        Builder { engine, abs_stack: vec![Vec::new()], taken: HashMap::new(), dirty: HashSet::new() }
    }

    pub fn build_root(mut self) -> Option<LayoutNode> {
        let html = self.engine.doc.document_element()?;
        let mut node = self.build_box(html)?;
        // Fixed / viewport-level absolutes hang off the root.
        let rest = self.abs_stack.pop().unwrap_or_default();
        node.children.extend(rest);
        Some(node)
    }

    fn style(&self, id: NodeId) -> Option<Arc<ComputedStyle>> {
        self.engine.style_of(id)
    }

    fn anon_block_style(&self, parent: &ComputedStyle) -> Arc<ComputedStyle> {
        let mut s = ComputedStyle::inherit_from(parent);
        s.display = Display::Block;
        Arc::new(s)
    }

    fn is_replaced(tag: &str) -> bool {
        matches!(tag, "img" | "canvas" | "video" | "svg" | "iframe" | "object" | "embed" | "rendrix-scene")
    }

    fn is_box_like(s: &ComputedStyle) -> bool {
        !s.background_color.is_transparent()
            || !s.background_images.is_empty()
            || s.border_width.iter().any(|w| *w > 0.0)
            || s.padding.iter().any(|p| *p != Dim::Px(0.0))
            || s.margin.iter().any(|m| *m != Dim::Px(0.0))
            || s.width != Dim::Auto
            || s.height != Dim::Auto
            || !s.box_shadow.is_empty()
            || s.has_transform()
            || s.opacity < 1.0
            || s.position != rendrix_css::Position::Static
    }

    fn has_block_child(&self, id: NodeId) -> bool {
        has_block_child(self.engine, id)
    }

    /// P1: reuse a whole persisted subtree when its computed style is unchanged and nothing
    /// inside it is dirty; otherwise rebuild the box (clean children are still reconciled from
    /// the pool, so a style-only change to a parent reuses them instead of rebuilding).
    /// Only called for children that generate a box under the *current* styles (collect()
    /// classifies first), so a display change can never reuse a stale shape.
    fn build_box_cached(&mut self, id: NodeId) -> Option<LayoutNode> {
        if !self.taken.is_empty() {
            if let Some(old) = self.taken.remove(&(id as u64)) {
                match self.style(id) {
                    None => return None, // display:none now (or removed): drop the stale box
                    Some(new_style) => {
                        if Arc::ptr_eq(&new_style, &old.style) && !self.dirty_at_or_inside(id) {
                            return Some(old);
                        }
                        // Shell changed (or something inside is dirty): offer the descendants
                        // for reuse and rebuild this box from the current styles.
                        drain_into(&mut self.taken, old);
                    }
                }
            }
        }
        self.build_box(id)
    }

    /// P1: true when this node or anything in its DOM subtree must be rebuilt.
    fn dirty_at_or_inside(&self, id: NodeId) -> bool {
        if self.dirty.is_empty() {
            return false;
        }
        if self.dirty.contains(&id) {
            return true;
        }
        let doc = &self.engine.doc;
        for d in &self.dirty {
            let mut cur = doc.parent(*d);
            while let Some(p) = cur {
                if p == id {
                    return true;
                }
                cur = doc.parent(p);
            }
        }
        false
    }

    /// Build the box for element `id`, or `None` if it generates none (`display: none`).
    pub fn build_box(&mut self, id: NodeId) -> Option<LayoutNode> {
        let engine = self.engine;
        let mut style = self.style(id)?;
        if style.display == Display::None {
            return None;
        }
        let tag: &str = engine.doc.tag(id)?;
        let lid = id as u64;

        // --- replaced elements ---
        if Self::is_replaced(tag) {
            let attr_px = |name: &str| engine.doc.get_attr(id, name).and_then(|v| v.trim().trim_end_matches("px").parse::<f32>().ok());
            let (mut w, mut h) = match tag {
                "img" => {
                    let natural = engine.doc.get_attr(id, "src").and_then(|src| engine.image_entry(src));
                    natural.map(|e| (e.width as f32, e.height as f32)).unwrap_or((0.0, 0.0))
                }
                // 3D scene host (`<rendrix-scene>`): a layout box with a scene viewport,
                // same intrinsic size as other replaced elements without natural size.
                "canvas" | "svg" | "iframe" | "video" | "object" | "embed" | "rendrix-scene" => (300.0, 150.0),
                _ => (0.0, 0.0),
            };
            if let Some(a) = attr_px("width") {
                w = a;
            }
            if let Some(a) = attr_px("height") {
                h = a;
            }
            // Explicit CSS sizes win; Replaced measurement preserves aspect ratio for the missing side.
            let mut s = (*style).clone();
            if s.width == Dim::Auto && s.height == Dim::Auto && (attr_px("width").is_some() || attr_px("height").is_some()) {
                if attr_px("width").is_some() {
                    s.width = Dim::Px(w);
                }
                if attr_px("height").is_some() {
                    s.height = Dim::Px(h);
                }
            }
            return Some(LayoutNode { id: lid, style: Arc::new(s), kind: LayoutKind::Replaced { width: w, height: h }, children: vec![] });
        }

        let positioned = style.position != rendrix_css::Position::Static || style.has_transform();
        let is_host = positioned;
        if is_host {
            self.abs_stack.push(Vec::new());
        }

        let (mut kind, mut children) = if let Some(content) = self.control_content(id, tag, &style) {
            (LayoutKind::Text(Arc::new(content)), vec![])
        } else if matches!(tag, "input") && !is_text_input(engine, id) {
            (LayoutKind::Box, vec![]) // checkbox, radio, hidden …
        } else {
            let flex = matches!(style.display, Display::Flex | Display::InlineFlex | Display::Grid | Display::InlineGrid);
            let mut items = Vec::new();
            self.collect(id, &style, flex, &mut items);
            self.arrange(id, &style, flex, items)
        };

        if is_host {
            children.extend(self.abs_stack.pop().unwrap_or_default());
        }
        // A folded text leaf cannot also own children (absolutely positioned descendants): split it.
        if matches!(kind, LayoutKind::Text(_)) && !children.is_empty() {
            if let LayoutKind::Text(content) = std::mem::replace(&mut kind, LayoutKind::Box) {
                let leaf_style = self.anon_block_style(&style);
                let content = Arc::new(InlineContent { runs: content.runs.clone(), container: style.clone() });
                children.insert(0, LayoutNode { id: anon_id(lid, 0), style: leaf_style, kind: LayoutKind::Text(content), children: vec![] });
            }
        }
        // Text controls and buttons keep their own style on the leaf.
        let _ = &mut style;
        Some(LayoutNode { id: lid, style, kind, children })
    }

    /// Content of form controls that render their own text (inputs, textareas, selects).
    fn control_content(&self, id: NodeId, tag: &str, style: &Arc<ComputedStyle>) -> Option<InlineContent> {
        let doc = &self.engine.doc;
        match tag {
            "input" if is_text_input(self.engine, id) => {
                let ty = doc.get_attr(id, "type").unwrap_or("text").to_ascii_lowercase();
                let value = doc.value(id);
                let mut container = (**style).clone();
                container.white_space = WhiteSpace::Pre;
                container.text_align = if matches!(style.text_align, TextAlign::Start) { TextAlign::Left } else { style.text_align };
                let container = Arc::new(container);
                let (text, run_style) = if value.is_empty() {
                    let mut ph = (*container).clone();
                    ph.color = rendrix_css::Color::rgba(0.46, 0.46, 0.46, 1.0);
                    (doc.get_attr(id, "placeholder").unwrap_or("").to_string(), Arc::new(ph))
                } else if ty == "password" {
                    ("•".repeat(value.chars().count()), container.clone())
                } else {
                    (value, container.clone())
                };
                Some(InlineContent { runs: vec![InlineRun { text, style: run_style, node: id as u64, is_break: false }], container })
            }
            "input" if matches!(doc.get_attr(id, "type").map(|t| t.to_ascii_lowercase()).as_deref(), Some("button" | "submit" | "reset")) => {
                let label = doc.get_attr(id, "value").unwrap_or(match doc.get_attr(id, "type") {
                    Some("reset") => "Reset",
                    Some("button") => "",
                    _ => "Submit",
                });
                let mut c = (**style).clone();
                c.white_space = WhiteSpace::Pre;
                let c = Arc::new(c);
                Some(InlineContent { runs: vec![InlineRun { text: label.to_string(), style: c.clone(), node: id as u64, is_break: false }], container: c })
            }
            "textarea" => {
                let value = doc.value(id);
                let (text, rs) = if value.is_empty() {
                    let mut ph = (**style).clone();
                    ph.color = rendrix_css::Color::rgba(0.46, 0.46, 0.46, 1.0);
                    (doc.get_attr(id, "placeholder").unwrap_or("").to_string(), Arc::new(ph))
                } else {
                    (value, style.clone())
                };
                Some(InlineContent { runs: vec![InlineRun { text, style: rs, node: id as u64, is_break: false }], container: style.clone() })
            }
            "select" => {
                let opts = doc.get_elements_by_tag_name(id, "option");
                let chosen = opts.iter().find(|o| doc.has_attr(**o, "selected")).or(opts.first());
                let label = chosen.map(|o| doc.text_content(*o).trim().to_string()).unwrap_or_default();
                let mut c = (**style).clone();
                c.white_space = WhiteSpace::Pre;
                let c = Arc::new(c);
                Some(InlineContent { runs: vec![InlineRun { text: format!("{label}  \u{25be}"), style: c.clone(), node: id as u64, is_break: false }], container: c })
            }
            _ => None,
        }
    }

    fn push_generated(&mut self, parent: NodeId, before: bool, flex: bool, items: &mut Vec<Item>) {
        let Some(ps) = self.engine.pseudo_style_of(parent, before) else { return };
        let Some(text) = ps.content.clone() else { return };
        if ps.display == Display::None {
            return;
        }
        let run = InlineRun { text, style: ps.clone(), node: parent as u64, is_break: false };
        if !flex && matches!(ps.display, Display::Inline) {
            items.push(Item::Run(run));
        } else {
            let content = Arc::new(InlineContent { runs: vec![run], container: ps.clone() });
            let id = anon_id(parent as u64, if before { 250 } else { 251 });
            items.push(Item::Block(LayoutNode { id, style: ps, kind: LayoutKind::Text(content), children: vec![] }));
        }
    }

    fn collect(&mut self, parent: NodeId, parent_style: &Arc<ComputedStyle>, flex: bool, items: &mut Vec<Item>) {
        let engine = self.engine;
        let doc = &engine.doc;
        self.push_generated(parent, true, flex, items);
        // List marker.
        if doc.tag(parent) == Some("li") && !parent_style.list_style_none {
            if let Some(list) = doc.parent(parent) {
                let marker = match doc.tag(list) {
                    Some("ol") => {
                        let start: i32 = doc.get_attr(list, "start").and_then(|s| s.parse().ok()).unwrap_or(1);
                        let idx = doc.child_elements(list).take_while(|c| *c != parent).filter(|c| doc.tag(*c) == Some("li")).count() as i32;
                        Some(format!("{}. ", start + idx))
                    }
                    Some("ul") | Some("menu") => Some("\u{2022} ".to_string()),
                    _ => None,
                };
                if let Some(m) = marker {
                    items.push(Item::Run(InlineRun { text: m, style: parent_style.clone(), node: parent as u64, is_break: false }));
                }
            }
        }
        // P5: iterate children directly instead of collecting ids first; the iterator borrows
        // the document through the engine's shared reference, so it stays valid across the
        // `&mut self` calls below.
        for c in doc.children(parent) {
            match &doc.node(c).kind {
                NodeKind::Text(t) => {
                    items.push(Item::Run(InlineRun { text: t.clone(), style: parent_style.clone(), node: parent as u64, is_break: false }));
                }
                NodeKind::Element(e) => {
                    let Some(cs) = self.style(c) else { continue };
                    match cs.display {
                        Display::None => continue,
                        Display::Contents => {
                            self.collect(c, &cs, flex, items);
                            continue;
                        }
                        _ => {}
                    }
                    if cs.is_out_of_flow() {
                        if let Some(n) = self.build_box_cached(c) {
                            let idx = if cs.position == rendrix_css::Position::Fixed { 0 } else { self.abs_stack.len() - 1 };
                            self.abs_stack[idx].push(n);
                        }
                        continue;
                    }
                    if e.tag == "br" {
                        items.push(Item::Run(InlineRun { text: String::new(), style: parent_style.clone(), node: parent as u64, is_break: true }));
                        continue;
                    }
                    if flex {
                        if let Some(n) = self.build_box_cached(c) {
                            items.push(Item::Block(n));
                        }
                        continue;
                    }
                    let tag = e.tag.as_str();
                    let inline_flow = cs.display == Display::Inline && !Self::is_replaced(tag) && !Self::is_box_like(&cs) && !self.has_block_child(c) && !matches!(tag, "input" | "button" | "select" | "textarea");
                    if inline_flow {
                        self.collect(c, &cs, false, items);
                    } else if matches!(cs.display, Display::Inline | Display::InlineBlock | Display::InlineFlex | Display::InlineGrid) {
                        if let Some(n) = self.build_box_cached(c) {
                            items.push(Item::Atomic(n));
                        }
                    } else if let Some(n) = self.build_box_cached(c) {
                        items.push(Item::Block(n));
                    }
                }
                _ => {}
            }
        }
        self.push_generated(parent, false, flex, items);
    }

    fn text_leaf(&self, parent: u64, idx: u64, style: &Arc<ComputedStyle>, runs: Vec<InlineRun>) -> LayoutNode {
        let leaf_style = self.anon_block_style(style);
        let content = Arc::new(InlineContent { runs, container: leaf_style.clone() });
        LayoutNode { id: anon_id(parent, idx), style: leaf_style, kind: LayoutKind::Text(content), children: vec![] }
    }

    fn arrange(&mut self, id: NodeId, style: &Arc<ComputedStyle>, flex: bool, items: Vec<Item>) -> (LayoutKind, Vec<LayoutNode>) {
        let lid = id as u64;
        let mut children: Vec<LayoutNode> = Vec::new();
        let mut anon_idx = 1u64;

        if flex {
            let mut buf: Vec<InlineRun> = Vec::new();
            let flush = |buf: &mut Vec<InlineRun>, children: &mut Vec<LayoutNode>, anon_idx: &mut u64, this: &Self| {
                let runs = collapse(std::mem::take(buf));
                if runs.iter().any(|r| r.is_break || !r.text.trim().is_empty()) {
                    children.push(this.text_leaf(lid, *anon_idx, style, runs));
                    *anon_idx += 1;
                }
            };
            for it in items {
                match it {
                    Item::Run(r) => buf.push(r),
                    Item::Atomic(n) | Item::Block(n) => {
                        flush(&mut buf, &mut children, &mut anon_idx, self);
                        children.push(n);
                    }
                }
            }
            flush(&mut buf, &mut children, &mut anon_idx, self);
            return (LayoutKind::Box, children);
        }

        let has_block = items.iter().any(|i| matches!(i, Item::Block(_)));
        let has_atomic = items.iter().any(|i| matches!(i, Item::Atomic(_)));
        if !has_block && !has_atomic {
            let runs: Vec<InlineRun> = items.into_iter().filter_map(|i| if let Item::Run(r) = i { Some(r) } else { None }).collect();
            let runs = collapse(runs);
            if runs.is_empty() || runs.iter().all(|r| !r.is_break && r.text.trim().is_empty() && r.style.white_space != WhiteSpace::Pre && r.style.white_space != WhiteSpace::PreWrap) {
                return (LayoutKind::Box, vec![]);
            }
            return (LayoutKind::Text(Arc::new(InlineContent { runs, container: style.clone() })), vec![]);
        }

        // Mixed content: group consecutive inline-level items into anonymous boxes.
        let mut group: Vec<Item> = Vec::new();
        let flush_group = |group: &mut Vec<Item>, children: &mut Vec<LayoutNode>, anon_idx: &mut u64, this: &Self| {
            if group.is_empty() {
                return;
            }
            let g = std::mem::take(group);
            let atomic = g.iter().any(|i| matches!(i, Item::Atomic(_)));
            if !atomic {
                let runs: Vec<InlineRun> = g.into_iter().filter_map(|i| if let Item::Run(r) = i { Some(r) } else { None }).collect();
                let runs = collapse(runs);
                if runs.iter().any(|r| r.is_break || !r.text.trim().is_empty()) {
                    children.push(this.text_leaf(lid, *anon_idx, style, runs));
                    *anon_idx += 1;
                }
                return;
            }
            // Inline-level atomic boxes (buttons, inputs, images, inline-blocks) flow like a wrapping row.
            let mut row_style = ComputedStyle::inherit_from(style);
            row_style.display = Display::Flex;
            row_style.flex_wrap = rendrix_css::FlexWrap::Wrap;
            row_style.align_items = Align::Center;
            row_style.justify_content = match style.text_align {
                TextAlign::Center => Align::Center,
                TextAlign::Right | TextAlign::End => Align::FlexEnd,
                _ => Align::FlexStart,
            };
            row_style.column_gap = Dim::Px(style.font_size * 0.28);
            let row_id = anon_id(lid, *anon_idx);
            *anon_idx += 1;
            let row_style = Arc::new(row_style);
            let mut kids: Vec<LayoutNode> = Vec::new();
            let mut run_buf: Vec<InlineRun> = Vec::new();
            let mut sub = 0u64;
            let mut flush_runs = |buf: &mut Vec<InlineRun>, kids: &mut Vec<LayoutNode>| {
                let runs = collapse(std::mem::take(buf));
                if runs.iter().any(|r| r.is_break || !r.text.trim().is_empty()) {
                    sub += 1;
                    let ls = this.anon_block_style(style);
                    let content = Arc::new(InlineContent { runs, container: ls.clone() });
                    kids.push(LayoutNode { id: anon_id(row_id, sub), style: ls, kind: LayoutKind::Text(content), children: vec![] });
                }
            };
            for it in g {
                match it {
                    Item::Run(r) => run_buf.push(r),
                    Item::Atomic(n) | Item::Block(n) => {
                        flush_runs(&mut run_buf, &mut kids);
                        kids.push(n);
                    }
                }
            }
            flush_runs(&mut run_buf, &mut kids);
            children.push(LayoutNode { id: row_id, style: row_style, kind: LayoutKind::Box, children: kids });
        };
        for it in items {
            match it {
                Item::Block(n) => {
                    flush_group(&mut group, &mut children, &mut anon_idx, self);
                    children.push(n);
                }
                other => group.push(other),
            }
        }
        flush_group(&mut group, &mut children, &mut anon_idx, self);
        (LayoutKind::Box, children)
    }
}

/// CSS white-space processing across a sequence of runs in one inline formatting context.
pub fn collapse(runs: Vec<InlineRun>) -> Vec<InlineRun> {
    let mut out: Vec<InlineRun> = Vec::with_capacity(runs.len());
    let mut prev_space = true; // leading whitespace at line start is removed
    for mut r in runs {
        if r.is_break {
            prev_space = true;
            // Trailing collapsible space before a forced break is removed.
            if let Some(last) = out.last_mut() {
                if !last.is_break && collapsible(last.style.white_space) && last.text.ends_with(' ') {
                    last.text.pop();
                }
            }
            out.push(r);
            continue;
        }
        match r.style.white_space {
            WhiteSpace::Pre | WhiteSpace::PreWrap => {
                prev_space = r.text.ends_with(['\n', ' ']);
                if !r.text.is_empty() {
                    out.push(r);
                }
            }
            ws => {
                let keep_nl = ws == WhiteSpace::PreLine;
                let mut s = String::with_capacity(r.text.len());
                for ch in r.text.chars() {
                    let is_ws = matches!(ch, ' ' | '\t' | '\n' | '\r' | '\u{c}');
                    if keep_nl && ch == '\n' {
                        s.push('\n');
                        prev_space = true;
                    } else if is_ws {
                        if !prev_space {
                            s.push(' ');
                            prev_space = true;
                        }
                    } else {
                        s.push(ch);
                        prev_space = false;
                    }
                }
                if !s.is_empty() {
                    r.text = s;
                    out.push(r);
                }
            }
        }
    }
    while let Some(last) = out.last_mut() {
        if !last.is_break && collapsible(last.style.white_space) && last.text.ends_with(' ') {
            last.text.pop();
            if last.text.is_empty() {
                out.pop();
                continue;
            }
        }
        break;
    }
    out
}

fn collapsible(ws: WhiteSpace) -> bool {
    matches!(ws, WhiteSpace::Normal | WhiteSpace::NoWrap | WhiteSpace::PreLine)
}

// ---------------------------------------------------------------------------------------------
// P1: incremental box generation.
// ---------------------------------------------------------------------------------------------
//
// The previous frame's box tree persists in `Engine::layout_tree`. On a style/structure change
// only the affected units are rebuilt: each dirty DOM node resolves to the box-parent of its
// box owner (so `arrange` re-runs in the same parent context as a full build), that unit is
// rebuilt with clean style-identical subtrees reused by NodeId, and the result is spliced back
// in. Anonymous ids stay deterministic, so the patched tree is structurally identical to a
// full `build_root` (asserted by the equivalence fuzz below).
//
// Soundness notes:
// - `build_box_cached` is only called for children that generate a box under the *current*
//   styles (collect() classifies first), so a display/category change always rebuilds.
// - Whole-subtree reuse additionally requires style `Arc` equality and no dirty node at or
//   inside the subtree. DOM mutations always mark dirty roots, so unmarked subtrees are frozen.
// - Out-of-flow boxes that would escape the rebuilt unit (fixed positioning, or absolutes
//   above a non-positioned unit) are detected through the builder's `abs_stack`: the patch
//   bails out (`false`) and the caller falls back to `build_root` — identical output.

/// P1: return a dropped box's descendants to the reuse pool (anonymous leaves always rebuild).
fn drain_into(pool: &mut HashMap<u64, LayoutNode>, mut node: LayoutNode) {
    for child in node.children.drain(..) {
        drain_into(pool, child);
    }
    if node.id & ANON == 0 {
        pool.insert(node.id, node);
    }
}

fn has_block_child(engine: &Engine, id: NodeId) -> bool {
    engine.doc.child_elements(id).any(|c| match engine.style_of(c) {
        Some(s) => match s.display {
            Display::Block | Display::Flex | Display::Grid => true,
            Display::Inline => has_block_child(engine, c),
            _ => false,
        },
        None => false,
    })
}

/// P1: does this element dissolve into its parent's inline content (or vanish structurally)?
fn is_transparent(engine: &Engine, id: NodeId, s: &Arc<ComputedStyle>) -> bool {
    if s.display == Display::Contents {
        return true;
    }
    if s.display != Display::Inline {
        return false;
    }
    let tag = match engine.doc.tag(id) {
        Some(t) => t,
        None => return false,
    };
    if matches!(tag, "input" | "button" | "select" | "textarea") {
        return false;
    }
    if Builder::is_replaced(tag) || Builder::is_box_like(s) {
        return false;
    }
    if has_block_child(engine, id) {
        return false;
    }
    true
}

/// P1: nearest box-generating ancestor-or-self of `start`, or `None` when styles are missing.
fn box_ancestor(engine: &Engine, mut start: Option<NodeId>) -> Option<NodeId> {
    while let Some(n) = start {
        if engine.doc.element(n).is_none() {
            start = engine.doc.parent(n);
            continue;
        }
        let s = engine.style_of(n)?;
        if is_transparent(engine, n, &s) {
            start = engine.doc.parent(n);
            continue;
        }
        return Some(n);
    }
    None
}

/// P1: candidate rebuild units for a dirty DOM node, innermost first: the box owner itself
/// when the dirty node generates a box (rebuilding it re-runs its own collect), otherwise the
/// box its contribution dissolves into, then successively outer boxes. The patcher takes the
/// first candidate present in the old tree, so a display change that alters grouping simply
/// escalates to the parent instead of falling back to a full rebuild.
fn unit_candidates(engine: &Engine, id: NodeId) -> Vec<NodeId> {
    let start = if engine.doc.element(id).is_some() { Some(id) } else { engine.doc.parent(id) };
    let mut out = Vec::new();
    let mut cur = box_ancestor(engine, start);
    while let Some(n) = cur {
        out.push(n);
        cur = box_ancestor(engine, engine.doc.parent(n));
    }
    out
}

fn contains(engine: &Engine, anc: NodeId, desc: NodeId) -> bool {
    let mut cur = Some(desc);
    while let Some(n) = cur {
        if n == anc {
            return true;
        }
        cur = engine.doc.parent(n);
    }
    false
}

fn find_node<'t>(root: &'t LayoutNode, id: u64) -> Option<&'t LayoutNode> {
    if root.id == id {
        return Some(root);
    }
    for c in &root.children {
        if let Some(found) = find_node(c, id) {
            return Some(found);
        }
    }
    None
}

fn find_node_mut<'t>(root: &'t mut LayoutNode, id: u64) -> Option<&'t mut LayoutNode> {
    if root.id == id {
        return Some(root);
    }
    for c in &mut root.children {
        if let Some(found) = find_node_mut(c, id) {
            return Some(found);
        }
    }
    None
}

fn remove_node(node: &mut LayoutNode, id: u64) -> bool {
    if let Some(pos) = node.children.iter().position(|c| c.id == id) {
        node.children.remove(pos);
        return true;
    }
    for c in &mut node.children {
        if remove_node(c, id) {
            return true;
        }
    }
    false
}

/// P1: remove the box with `id` from the old tree and pool it (with descendants) for reuse.
fn take_unit_node(root: &mut LayoutNode, id: u64, pool: &mut HashMap<u64, LayoutNode>) -> bool {
    if let Some(pos) = root.children.iter().position(|c| c.id == id) {
        let node = root.children.remove(pos);
        drain_into(pool, node);
        return true;
    }
    for c in &mut root.children {
        if take_unit_node(c, id, pool) {
            return true;
        }
    }
    false
}

/// P1: patch the previous frame's box tree in place. Returns `false` when the patch cannot
/// preserve full-build semantics (stale shape, escaping out-of-flow boxes); the caller then
/// discards the tree and runs `build_root`.
pub(crate) fn patch_tree(engine: &Engine, old_root: &mut LayoutNode, dirty_ids: &[NodeId]) -> bool {
    // Resolve dirty DOM nodes to rebuild units (innermost candidate present in the old tree),
    // dropping units nested inside another unit.
    let mut units: Vec<NodeId> = Vec::new();
    for id in dirty_ids {
        if !engine.doc.is_alive(*id) || !engine.doc.is_connected(*id) {
            continue; // removed; the surviving parent covers it via its own dirty root
        }
        let mut chosen = None;
        for cand in unit_candidates(engine, *id) {
            if old_root.id == cand as u64 || find_node(old_root, cand as u64).is_some() {
                chosen = Some(cand);
                break;
            }
        }
        let Some(u) = chosen else { return false };
        if units.iter().any(|kept| contains(engine, *kept, u)) {
            continue;
        }
        units.retain(|kept| !contains(engine, u, *kept));
        units.push(u);
    }
    if units.is_empty() {
        return true;
    }
    let dirty: HashSet<NodeId> = dirty_ids.iter().copied().collect();
    let mut pool: HashMap<u64, LayoutNode> = HashMap::new();
    for u in units {
        if old_root.id == u as u64 {
            let children = std::mem::take(&mut old_root.children);
            for ch in children {
                drain_into(&mut pool, ch);
            }
        } else if !take_unit_node(old_root, u as u64, &mut pool) {
            return false; // unit not in the old tree: full rebuild
        }
        let mut builder = Builder { engine, abs_stack: vec![Vec::new()], taken: std::mem::take(&mut pool), dirty: dirty.clone() };
        let rebuilt = builder.build_box_cached(u);
        if !builder.abs_stack[0].is_empty() {
            return false; // escaping out-of-flow boxes: full rebuild re-hangs them
        }
        pool = std::mem::take(&mut builder.taken);
        match rebuilt {
            Some(node) => {
                if old_root.id == u as u64 {
                    *old_root = node;
                } else if let Some(slot) = find_node_mut(old_root, u as u64) {
                    *slot = node;
                } else {
                    return false;
                }
            }
            None => {
                // The whole unit is now `display: none`: drop it.
                if old_root.id == u as u64 {
                    return false; // root `display: none`: let build_root produce None
                }
                remove_node(old_root, u as u64);
            }
        }
    }
    true
}

/// P1: keep a reused root box's viewport width in sync when boxes survive a resize.
pub(crate) fn refresh_root_width(root: &mut LayoutNode, viewport_w: f32) {
    if root.style.width != Dim::Px(viewport_w) {
        let mut s = (*root.style).clone();
        s.width = Dim::Px(viewport_w);
        root.style = Arc::new(s);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(t: &str) -> InlineRun {
        InlineRun { text: t.into(), style: Arc::new(ComputedStyle::default()), node: 1, is_break: false }
    }

    fn text(runs: &[InlineRun]) -> String {
        runs.iter().map(|r| if r.is_break { "|".to_string() } else { r.text.clone() }).collect()
    }

    #[test]
    fn collapses_whitespace_across_runs() {
        let out = collapse(vec![run("  hello \n  "), run(" world  "), run(" !")]);
        assert_eq!(text(&out), "hello world !");
    }

    #[test]
    fn trims_edges_and_breaks() {
        let br = InlineRun { text: String::new(), style: Arc::new(ComputedStyle::default()), node: 1, is_break: true };
        let out = collapse(vec![run("a  "), br, run("   b  ")]);
        assert_eq!(text(&out), "a|b");
        assert!(collapse(vec![run("   \n  ")]).is_empty());
    }

    #[test]
    fn preformatted_text_is_kept() {
        let mut s = ComputedStyle::default();
        s.white_space = WhiteSpace::Pre;
        let r = InlineRun { text: "  a\n  b ".into(), style: Arc::new(s), node: 1, is_break: false };
        assert_eq!(text(&collapse(vec![r])), "  a\n  b ");
    }

    #[test]
    fn nbsp_is_not_collapsed() {
        assert_eq!(text(&collapse(vec![run("a\u{a0}\u{a0}b")])), "a\u{a0}\u{a0}b");
    }

    // P1 equivalence fuzz: the incrementally patched tree must equal a full rebuild
    // (structural + computed-style equality) across inserts, removes, moves, class, text,
    // `:nth-child` and abs-pos mutations. The reference engine forces a full rebuild every
    // frame via `mark_all_style_dirty`; geometry is compared through the public rect API.
    mod patch_equivalence {
        use crate::{Engine, MapAssets};
        use rendrix_css::ComputedStyle;
        use rendrix_dom::Document;
        use rendrix_layout::{LayoutKind, LayoutNode};
        use std::sync::Arc;

        const HTML: &str = r#"<div id="list"><div class="item" id="i0"><span class="label">zero</span><button>Go</button></div><div class="item" id="i1"><span class="label">one</span></div><div class="item" id="i2"><span class="label">two</span></div></div><ul id="nums"><li id="l0">a</li><li id="l1">b</li></ul><div id="stage"><div id="abs">float</div></div><div id="fix">fixed</div><p id="para">Hello <b id="bold">bold</b> world</p><input id="name" value="hi">"#;
        const CSS: &str = "body{margin:0} #list{display:flex;flex-direction:column;gap:4px} .item{display:flex;background:#eee} .item.hot{background:#cfc} .label{flex:1} li{height:10px} li:first-child{color:rgb(1,1,1)} li:nth-child(2){font-weight:700} #stage{position:relative;width:300px;height:200px} #abs{position:absolute;right:10px;bottom:10px;width:50px;height:50px} #fix{position:fixed;top:0;left:0;width:20px;height:20px} #para{width:200px} #para.q::before{content:'['} #para.q::after{content:']'}";
        const IDS: &[&str] = &["i0", "i1", "i2", "l0", "l1", "abs", "fix", "para", "bold", "name", "list", "stage"];

        fn pair() -> (Engine, Engine) {
            let mk = || {
                let src = format!("<html><head><style>{CSS}</style></head><body>{HTML}</body></html>");
                let mut e = Engine::with_document(Document::parse(&src), Box::new(MapAssets::default()));
                e.set_viewport(800.0, 600.0, 1.0);
                e
            };
            let (mut a, mut b) = (mk(), mk());
            a.update(0.0);
            b.doc.mark_all_style_dirty();
            b.update(0.0);
            (a, b)
        }

        fn assert_same_tree(a: &LayoutNode, b: &LayoutNode) {
            assert_eq!(a.id, b.id, "box id");
            assert_eq!(a.style.as_ref(), b.style.as_ref(), "computed style of box {}", a.id);
            match (&a.kind, &b.kind) {
                (LayoutKind::Box, LayoutKind::Box) => {}
                (LayoutKind::Text(x), LayoutKind::Text(y)) => {
                    assert_eq!(x.runs.len(), y.runs.len(), "run count of box {}", a.id);
                    for (r1, r2) in x.runs.iter().zip(y.runs.iter()) {
                        assert_eq!((r1.text.as_str(), r1.is_break, r1.node), (r2.text.as_str(), r2.is_break, r2.node));
                        assert_eq!(r1.style.as_ref(), r2.style.as_ref());
                    }
                    assert_eq!(x.container.as_ref(), y.container.as_ref());
                }
                (
                    LayoutKind::Replaced { width: w1, height: h1 },
                    LayoutKind::Replaced { width: w2, height: h2 },
                ) => assert_eq!((*w1, *h1), (*w2, *h2)),
                _ => panic!("kind mismatch for box {}", a.id),
            }
            assert_eq!(a.children.len(), b.children.len(), "children of box {}", a.id);
            for (ca, cb) in a.children.iter().zip(b.children.iter()) {
                assert_same_tree(ca, cb);
            }
        }

        fn style_ptr(e: &Engine, id: &str) -> *const ComputedStyle {
            let dom = e.doc.get_element_by_id(id).unwrap();
            fn find(n: &LayoutNode, id: u64) -> Option<&LayoutNode> {
                if n.id == id {
                    return Some(n);
                }
                n.children.iter().find_map(|c| find(c, id))
            }
            let tree = e.layout_tree.as_ref().unwrap();
            Arc::as_ptr(&find(tree, dom as u64).unwrap().style)
        }

        fn step(a: &mut Engine, b: &mut Engine, t: f64) {
            b.doc.mark_all_style_dirty();
            a.update(t);
            b.update(t);
            assert_same_tree(a.layout_tree.as_ref().unwrap(), b.layout_tree.as_ref().unwrap());
            for id in IDS {
                let ra = a.doc.get_element_by_id(id).and_then(|n| a.element_rect(n));
                let rb = b.doc.get_element_by_id(id).and_then(|n| b.element_rect(n));
                assert_eq!(ra, rb, "rect of #{id}");
            }
        }

        fn mutate_both(a: &mut Engine, b: &mut Engine, f: impl Fn(&mut Document)) {
            f(&mut a.doc);
            f(&mut b.doc);
        }

        #[test]
        fn patched_tree_matches_full_rebuild_across_mutations() {
            let (mut a, mut b) = pair();
            let mut t = 0.1;
            let mut go = |a: &mut Engine, b: &mut Engine| {
                t += 0.1;
                step(a, b, t);
            };

            // 1. class toggle: style-only change to one item.
            let (p0, p1) = (style_ptr(&a, "i0"), style_ptr(&a, "i1"));
            mutate_both(&mut a, &mut b, |d| {
                let i1 = d.get_element_by_id("i1").unwrap();
                d.toggle_class(i1, "hot", None);
            });
            go(&mut a, &mut b);
            assert_eq!(style_ptr(&a, "i0"), p0, "clean sibling box is reused, not rebuilt");
            assert_ne!(style_ptr(&a, "i1"), p1, "changed box gets a fresh style");

            // 2. text change inside a flex item.
            mutate_both(&mut a, &mut b, |d| {
                let label = d.query_selector(d.get_element_by_id("i0").unwrap(), ".label").unwrap().unwrap();
                d.set_text_content(label, "zero updated with a longer line");
            });
            go(&mut a, &mut b);

            // 3. append a new item (new box under a reused parent).
            mutate_both(&mut a, &mut b, |d| {
                let list = d.get_element_by_id("list").unwrap();
                let it = d.create_element("div");
                d.set_attr(it, "class", "item");
                d.set_attr(it, "id", "i3");
                let s = d.create_element("span");
                d.set_attr(s, "class", "label");
                let tx = d.create_text("three");
                d.append_child(s, tx).unwrap();
                d.append_child(it, s).unwrap();
                d.append_child(list, it).unwrap();
            });
            go(&mut a, &mut b);

            // 4. prepend to the nth-child list (structural restyle of siblings).
            mutate_both(&mut a, &mut b, |d| {
                let nums = d.get_element_by_id("nums").unwrap();
                let li = d.create_element("li");
                let tx = d.create_text("z");
                d.append_child(li, tx).unwrap();
                let first = d.children(nums).next();
                d.insert_before(nums, li, first).unwrap();
            });
            go(&mut a, &mut b);

            // 5. remove an item.
            mutate_both(&mut a, &mut b, |d| {
                let i2 = d.get_element_by_id("i2").unwrap();
                d.remove(i2);
            });
            go(&mut a, &mut b);

            // 6. move a node (re-parent under the same list).
            mutate_both(&mut a, &mut b, |d| {
                let (list, i0) = (d.get_element_by_id("list").unwrap(), d.get_element_by_id("i0").unwrap());
                d.append_child(list, i0).unwrap();
            });
            go(&mut a, &mut b);

            // 7. inline display change regroups the paragraph (block <-> inline).
            mutate_both(&mut a, &mut b, |d| {
                let bold = d.get_element_by_id("bold").unwrap();
                d.set_attr(bold, "style", "display:block");
            });
            go(&mut a, &mut b);
            mutate_both(&mut a, &mut b, |d| {
                let bold = d.get_element_by_id("bold").unwrap();
                d.set_attr(bold, "style", "display:inline");
            });
            go(&mut a, &mut b);

            // 8. abs-pos child style change (contained: stays incremental).
            mutate_both(&mut a, &mut b, |d| {
                let abs = d.get_element_by_id("abs").unwrap();
                d.set_attr(abs, "style", "left:5px;top:5px");
            });
            go(&mut a, &mut b);

            // 9. form control value (control-content path).
            mutate_both(&mut a, &mut b, |d| {
                let name = d.get_element_by_id("name").unwrap();
                d.set_form_value(name, "hello world");
            });
            go(&mut a, &mut b);

            // 10. generated content toggled by a class.
            mutate_both(&mut a, &mut b, |d| {
                let para = d.get_element_by_id("para").unwrap();
                d.toggle_class(para, "q", Some(true));
            });
            go(&mut a, &mut b);

            // 11. paragraph children replaced wholesale (stale inline boxes dropped).
            mutate_both(&mut a, &mut b, |d| {
                let para = d.get_element_by_id("para").unwrap();
                d.set_text_content(para, "Hello world, rewritten");
            });
            go(&mut a, &mut b);

            // 12. fixed-position text change (escapes the unit: falls back to full build).
            mutate_both(&mut a, &mut b, |d| {
                let fix = d.get_element_by_id("fix").unwrap();
                d.set_text_content(fix, "fixed!");
            });
            go(&mut a, &mut b);

            // 13. viewport resize with neutral CSS: P3 skips restyle, boxes are reused.
            a.set_viewport(750.0, 600.0, 1.0);
            b.set_viewport(750.0, 600.0, 1.0);
            go(&mut a, &mut b);
        }
    }
}
