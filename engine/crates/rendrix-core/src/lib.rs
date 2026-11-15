//! Rendrix core: the engine that turns a DOM into pixels.
//!
//! `Engine::update` runs the pipeline: stylesheets → restyle → box tree → layout → display list.
//! Interaction (hit testing, scrolling, text editing) works against the geometry of the last frame.
//! The engine knows nothing about windows, GPUs or JavaScript; those layers sit above it.

mod assets;
mod boxtree;
mod edit;
mod paint;
mod style;

pub use assets::{AssetProvider, DirAssets, ImageEntry, MapAssets};
pub use edit::{EditOp, EditResult};
pub use paint::{HitEntry, ScrollInfo};

use assets::Images;
use rendrix_css::animation::mat_apply;
use rendrix_css::{Color, ComputedStyle, Cursor, RuleSet};
use rendrix_dom::{Document, NodeId};
use rendrix_gfx::{DisplayList, TextSystem};
use rendrix_layout::{InlineContent, LayoutEngine, LayoutNode, LayoutResult, TaffyEngine, TextMeasurer};
use std::cell::{Ref, RefCell, RefMut};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

/// Adapter so layout can measure text through the engine's shared text system.
struct Measurer<'a>(&'a RefCell<TextSystem>);

impl TextMeasurer for Measurer<'_> {
    fn measure(&mut self, content: &InlineContent, max_width: Option<f32>) -> (f32, f32) {
        self.0.borrow_mut().measure(content, max_width)
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq)]
pub struct UpdateInfo {
    /// A new display list was produced.
    pub repainted: bool,
    /// Transitions or animations are running: call `update` again next frame.
    pub animating: bool,
    /// Absolute time (engine seconds) at which something time-driven (caret blink) needs a frame.
    pub next_wake: Option<f64>,
}

#[derive(Debug, Default, Clone)]
pub struct EngineStats {
    pub frames: u64,
    pub restyles: u64,
    pub layouts: u64,
    pub full_restyles: u64,
    pub last_layout_micros: u64,
    pub last_style_micros: u64,
    pub last_paint_micros: u64,
    /// M0 shaping signals (populated per frame; Taffy new/restyled node counts
    /// stay inside `TaffyEngine` until its layout API exposes them).
    pub shape_count: usize,
    pub cache_hits: usize,
    pub cache_len: usize,
}

pub struct Engine {
    pub doc: Document,
    pub(crate) text: RefCell<TextSystem>,
    layout: TaffyEngine,
    pub(crate) assets: Box<dyn AssetProvider>,
    pub(crate) images: RefCell<Images>,
    pub viewport: (f32, f32),
    pub scale: f32,
    pub dark: bool,
    /// Safe-area insets (top, right, bottom, left) in CSS px, exposed as `env(safe-area-inset-*)`.
    pub safe_area: [f32; 4],
    /// On-screen keyboard height in CSS px, exposed as `env(keyboard-inset-height)`.
    pub keyboard_h: f32,

    pub(crate) rules: RuleSet,
    pub(crate) rules_dirty: bool,
    pub(crate) rules_media_sig: Option<(u32, u32, u32, bool)>,
    pub(crate) adopted_sheets: Vec<String>,
    pub(crate) loaded_fonts: HashSet<String>,
    pub(crate) styles: Vec<Option<Arc<ComputedStyle>>>,
    pub(crate) pseudo: HashMap<(NodeId, bool), Arc<ComputedStyle>>,
    pub(crate) anim: HashMap<NodeId, style::AnimState>,
    pub now: f64,

    /// P1: the previous frame's box tree persists across `update` calls so dirty regions can
    /// be rebuilt and spliced by NodeId instead of rebuilding the whole tree.
    pub(crate) layout_tree: Option<LayoutNode>,
    layout_result: LayoutResult,
    display: DisplayList,
    hits: Vec<HitEntry>,
    scrolls: Vec<ScrollInfo>,
    rects: HashMap<NodeId, [f32; 4]>,
    scroll: RefCell<HashMap<NodeId, (f32, f32)>>,
    input_scroll: RefCell<HashMap<NodeId, f32>>,
    caret_epoch: f64,
    last_caret_visible: bool,
    viewport_changed: bool,
    force_repaint: bool,
    pub(crate) style_changed: bool,
    last_gc: f64,
    freed_log: Vec<NodeId>,
    pub stats: EngineStats,
}

impl Engine {
    pub fn new(assets: Box<dyn AssetProvider>) -> Engine {
        Engine::with_document(Document::new(), assets)
    }

    pub fn with_document(doc: Document, assets: Box<dyn AssetProvider>) -> Engine {
        Engine {
            doc,
            text: RefCell::new(TextSystem::new()),
            layout: TaffyEngine::new(),
            assets,
            images: RefCell::new(Images::default()),
            viewport: (800.0, 600.0),
            scale: 1.0,
            dark: false,
            safe_area: [0.0; 4],
            keyboard_h: 0.0,
            rules: RuleSet::new(),
            rules_dirty: true,
            rules_media_sig: None,
            adopted_sheets: Vec::new(),
            loaded_fonts: HashSet::new(),
            styles: Vec::new(),
            pseudo: HashMap::new(),
            anim: HashMap::new(),
            now: 0.0,
            layout_tree: None,
            layout_result: LayoutResult::default(),
            display: DisplayList::new(800.0, 600.0, Color::WHITE),
            hits: Vec::new(),
            scrolls: Vec::new(),
            rects: HashMap::new(),
            scroll: RefCell::new(HashMap::new()),
            input_scroll: RefCell::new(HashMap::new()),
            caret_epoch: 0.0,
            last_caret_visible: false,
            viewport_changed: true,
            force_repaint: true,
            style_changed: false,
            last_gc: 0.0,
            freed_log: Vec::new(),
            stats: EngineStats::default(),
        }
    }

    pub fn set_viewport(&mut self, width: f32, height: f32, scale: f32) {
        if (width, height, scale) != (self.viewport.0, self.viewport.1, self.scale) {
            self.viewport = (width, height);
            self.scale = scale;
            self.viewport_changed = true;
        }
    }

    /// Safe-area insets in CSS px: `[top, right, bottom, left]`.
    pub fn set_safe_area(&mut self, insets: [f32; 4]) {
        if insets != self.safe_area {
            self.safe_area = insets;
            self.viewport_changed = true;
        }
    }

    /// On-screen keyboard height in CSS px (0 when hidden).
    pub fn set_keyboard_inset(&mut self, height: f32) {
        if height != self.keyboard_h {
            self.keyboard_h = height;
            self.viewport_changed = true;
        }
    }

    pub fn set_dark(&mut self, dark: bool) {
        if self.dark != dark {
            self.dark = dark;
            self.rules_dirty = true;
        }
    }

    /// Add a constructed stylesheet (from `adoptedStyleSheets`/framework injection).
    pub fn add_stylesheet(&mut self, css: &str) {
        self.adopted_sheets.push(css.to_string());
        self.rules_dirty = true;
    }

    pub fn text_borrow(&self) -> RefMut<'_, TextSystem> {
        self.text.borrow_mut()
    }

    pub fn text_system(&self) -> Ref<'_, TextSystem> {
        self.text.borrow()
    }

    pub fn load_font(&self, data: Vec<u8>) {
        self.text.borrow_mut().load_font(data);
    }

    pub fn image_entry(&self, url: &str) -> Option<ImageEntry> {
        self.images.borrow_mut().get(url, self.assets.as_ref())
    }

    /// Rasterized inline `<svg>` for `node` at `w`×`h` px (cached by markup + size).
    pub fn inline_svg_entry(&self, node: NodeId, html: &str, w: u32, h: u32) -> Option<ImageEntry> {
        self.images.borrow_mut().svg(node, html, w, h)
    }

    /// Ids of nodes freed since the last call (so script-side state keyed by id can be dropped).
    pub fn take_freed_for_js(&mut self) -> Vec<NodeId> {
        std::mem::take(&mut self.freed_log)
    }

    /// Images decoded since the last call; the renderer must upload them under their ids.
    pub fn take_image_uploads(&self) -> Vec<ImageEntry> {
        std::mem::take(&mut self.images.borrow_mut().pending_upload)
    }

    pub fn read_asset(&self, path: &str) -> Option<Vec<u8>> {
        self.assets.read(path)
    }

    pub fn display_list(&self) -> &DisplayList {
        &self.display
    }

    pub fn caret_geometry(&self, content: &InlineContent, width: Option<f32>, byte: usize) -> (f32, f32, f32) {
        self.text.borrow_mut().caret(content, width, byte)
    }

    pub fn input_scroll(&self, id: NodeId, caret_x: f32, width: f32) -> f32 {
        let mut m = self.input_scroll.borrow_mut();
        let cur = m.entry(id).or_insert(0.0);
        if caret_x - *cur > width - 2.0 {
            *cur = caret_x - (width - 2.0);
        }
        if caret_x < *cur {
            *cur = caret_x;
        }
        *cur = cur.max(0.0);
        *cur
    }

    pub fn scroll_offset(&self, id: NodeId, max_x: f32, max_y: f32) -> (f32, f32) {
        let m = self.scroll.borrow();
        let (x, y) = m.get(&id).copied().unwrap_or((0.0, 0.0));
        (x.clamp(0.0, max_x), y.clamp(0.0, max_y))
    }

    pub fn set_scroll(&mut self, id: NodeId, x: f32, y: f32) {
        self.scroll.borrow_mut().insert(id, (x.max(0.0), y.max(0.0)));
        self.force_repaint = true;
    }

    pub fn get_scroll(&self, id: NodeId) -> (f32, f32) {
        let m = self.scroll.borrow().get(&id).copied().unwrap_or((0.0, 0.0));
        let max = self.scrolls.iter().find(|s| s.node == id).map(|s| (s.max_x, s.max_y)).unwrap_or((0.0, 0.0));
        (m.0.clamp(0.0, max.0), m.1.clamp(0.0, max.1))
    }

    /// Scroll extents (scrollWidth, scrollHeight, clientWidth, clientHeight) of an element.
    pub fn scroll_metrics(&self, id: NodeId) -> Option<(f32, f32, f32, f32)> {
        let b = self.layout_result.boxes.get(&(id as u64))?;
        let cw = b.width - b.border[1] - b.border[3];
        let ch = b.height - b.border[0] - b.border[2];
        Some((b.content_width.max(cw), b.content_height.max(ch), cw, ch))
    }

    // ------------------------------------------------------------------------------------
    // The pipeline
    // ------------------------------------------------------------------------------------

    pub fn caret_visible(&self) -> bool {
        ((self.now - self.caret_epoch) * 2.0).floor() as i64 % 2 == 0
    }

    /// Restart caret blinking (call on any editing or focus change).
    pub fn reset_caret(&mut self) {
        self.caret_epoch = self.now;
        self.force_repaint = true;
    }

    fn focused_text_control(&self) -> Option<NodeId> {
        self.doc.focused().filter(|n| self.doc.is_text_control(*n))
    }

    /// DOM, stylesheet or viewport changes waiting to be applied (ignores running animations, which
    /// advance by themselves each frame).
    pub fn has_pending_changes(&self) -> bool {
        let d = &self.doc.dirty;
        d.all_style || d.layout || d.paint || !d.style_roots.is_empty() || !d.state_roots.is_empty() || self.doc.sheets_dirty || self.rules_dirty || self.viewport_changed || self.force_repaint
    }

    /// Does the document need a new frame? Cheap check used by the frame scheduler.
    pub fn needs_update(&self) -> bool {
        let d = &self.doc.dirty;
        d.all_style || d.layout || d.paint || !d.style_roots.is_empty() || !d.state_roots.is_empty() || self.doc.sheets_dirty || self.rules_dirty || self.viewport_changed || self.force_repaint || !self.anim.is_empty()
    }

    pub fn update(&mut self, now: f64) -> UpdateInfo {
        self.now = now;
        self.stats.last_style_micros = 0;
        self.stats.last_layout_micros = 0;
        self.stats.last_paint_micros = 0;
        let t0 = web_time::Instant::now();
        let sheets_were_dirty = self.doc.sheets_dirty || self.rules_dirty;
        let rules_rebuilt = self.refresh_rules();
        // Sweep detached, unreferenced subtrees at most once a second.
        if now - self.last_gc >= 1.0 {
            self.last_gc = now;
            self.doc.collect_garbage();
        }
        // Nodes freed by the DOM must not leave stale caches behind.
        for id in self.doc.take_freed() {
            self.freed_log.push(id);
            if let Some(s) = self.styles.get_mut(id as usize) {
                *s = None;
            }
            self.anim.remove(&id);
            self.pseudo.remove(&(id, true));
            self.pseudo.remove(&(id, false));
            self.scroll.borrow_mut().remove(&id);
        }
        let dirty = self.doc.take_dirty();
        // Subtrees that were detached lose their cached styles: re-attaching restyles them in context.
        for root in &dirty.detached {
            for n in self.doc.depth_first(*root) {
                if let Some(s) = self.styles.get_mut(n as usize) {
                    *s = None;
                }
                self.anim.remove(&n);
                self.pseudo.remove(&(n, true));
                self.pseudo.remove(&(n, false));
            }
        }
        // P3 phase-1: a rebuild caused only by the media-context change (resize, DPR, dark
        // mode) with no `@media` rules, no viewport-relative declarations and no inline
        // viewport styles provably changes no computed style: percentages resolve at layout
        // time, never in the cascade. Skip the restyle; layout still reruns below.
        let viewport_neutral =
            !self.rules.has_media_rules && !self.rules.uses_viewport_units && !self.doc.inline_viewport_units;
        let media_only_rebuild = rules_rebuilt && !sheets_were_dirty && !dirty.all_style;
        let mut all = dirty.all_style || rules_rebuilt || self.viewport_changed;
        if media_only_rebuild && self.viewport_changed && viewport_neutral {
            all = dirty.all_style;
        }
        let mut restyled = false;
        if all || !dirty.style_roots.is_empty() || !dirty.state_roots.is_empty() {
            restyled = self.restyle(all, dirty.style_roots.clone(), dirty.state_roots.clone());
            self.stats.restyles += 1;
            if all {
                self.stats.full_restyles += 1;
            }
        }
        self.stats.last_style_micros = t0.elapsed().as_micros() as u64;
        let animating = self.tick_motion(now) || !self.anim.is_empty();

        // Caret blink toggles need a repaint without any DOM change.
        let caret_state = self.focused_text_control().is_some() && self.caret_visible();
        let caret_toggled = self.focused_text_control().is_some() && caret_state != self.last_caret_visible;
        self.last_caret_visible = caret_state;

        let needs = restyled || dirty.layout || dirty.paint || self.viewport_changed || self.layout_tree.is_none() || animating || caret_toggled || self.force_repaint;
        if !needs {
            return UpdateInfo { repainted: false, animating: false, next_wake: self.next_wake() };
        }
        let relayout = restyled || dirty.layout || self.viewport_changed || self.layout_tree.is_none() || animating;
        let vp_changed = self.viewport_changed;
        self.viewport_changed = false;
        self.force_repaint = false;

        if relayout {
            let t1 = web_time::Instant::now();
            // P1: incremental box generation. The previous frame's tree persists in
            // `layout_tree`; dirty regions are rebuilt and spliced by NodeId while clean
            // style-identical subtrees are reused. Falls back to `build_root` whenever the
            // patch cannot preserve full-build semantics (then output is still identical).
            let mut patched = false;
            if !all && !animating {
                if let Some(mut tree) = self.layout_tree.take() {
                    let mut roots: Vec<NodeId> =
                        dirty.style_roots.iter().chain(dirty.state_roots.iter()).copied().collect();
                    roots.retain(|n| self.doc.is_alive(*n) && self.doc.is_connected(*n));
                    if roots.is_empty() {
                        // No style-affecting change (e.g. a viewport-only resize whose
                        // restyle P3 skipped): styles — hence boxes — are identical, so the
                        // persisted tree is reused and only Taffy re-solves.
                        patched = true;
                    } else {
                        patched = boxtree::patch_tree(self, &mut tree, &roots);
                    }
                    if patched {
                        if vp_changed {
                            boxtree::refresh_root_width(&mut tree, self.viewport.0);
                        }
                        self.layout_result = self.layout.layout(&tree, self.viewport, &mut Measurer(&self.text));
                        self.layout_tree = Some(tree);
                        self.apply_float_clear();
                    }
                    // else: `tree` may have been partially drained into the reuse pool, so it
                    // is dropped here and the full rebuild below replaces it.
                }
            }
            if !patched {
                let tree = boxtree::Builder::new(self).build_root();
                if let Some(mut tree) = tree {
                    // The root fills the viewport width.
                    let mut s = (*tree.style).clone();
                    s.width = rendrix_css::Dim::Px(self.viewport.0);
                    tree.style = Arc::new(s);
                    self.layout_result = self.layout.layout(&tree, self.viewport, &mut Measurer(&self.text));
                    self.layout_tree = Some(tree);
                    self.apply_float_clear();
                } else {
                    self.layout_tree = None;
                    self.layout_result = LayoutResult::default();
                }
            }
            self.stats.layouts += 1;
            self.stats.last_layout_micros = t1.elapsed().as_micros() as u64;
        }

        let t2 = web_time::Instant::now();
        self.paint();
        self.stats.last_paint_micros = t2.elapsed().as_micros() as u64;
        {
            let text = self.text.borrow();
            self.stats.shape_count = text.shape_count;
            self.stats.cache_hits = text.cache_hits;
            self.stats.cache_len = text.cache_len();
        }
        self.stats.frames += 1;
        UpdateInfo { repainted: true, animating, next_wake: self.next_wake() }
    }

    fn next_wake(&self) -> Option<f64> {
        if self.focused_text_control().is_some() {
            let phase = (self.now - self.caret_epoch) * 2.0;
            Some(self.caret_epoch + (phase.floor() + 1.0) / 2.0)
        } else {
            None
        }
    }

    fn paint(&mut self) {
        let Some(tree) = self.layout_tree.take() else {
            self.display = DisplayList::new(self.viewport.0, self.viewport.1, Color::WHITE);
            self.hits.clear();
            return;
        };
        let html = self.doc.document_element().unwrap_or(0);
        let body = self.doc.body();
        let html_bg = self.style_of(html).map(|s| s.background_color).unwrap_or(Color::TRANSPARENT);
        let (bg, skip_body) = if html_bg.a > 0.0 {
            (html_bg, None)
        } else {
            match body.and_then(|b| self.style_of(b).map(|s| (b, s.background_color))) {
                Some((b, c)) if c.a > 0.0 => (c, Some(b)),
                _ => (Color::WHITE, None),
            }
        };
        // An opaque canvas colour: composite translucent backgrounds onto white.
        let bg = if bg.a < 1.0 { Color::WHITE.lerp(Color { a: 1.0, ..bg }, bg.a) } else { bg };
        let list = DisplayList::new(self.viewport.0, self.viewport.1, bg);
        let mut painter = paint::Painter::new(self, &self.layout_result, list, html);
        painter.set_skip_body_background(skip_body);
        painter.paint_root(&tree);
        let paint::Painter { list, hits, scrolls, .. } = painter;
        let mut rects = HashMap::new();
        for h in &hits {
            rects.entry(h.node).or_insert(h.rect);
        }
        self.display = list;
        self.hits = hits;
        self.scrolls = scrolls;
        self.rects = rects;
        self.layout_tree = Some(tree);
    }

    /// Advance the engine clock without producing a frame (so style/geometry reads from script see
    /// transitions and animations at the current time).
    pub fn set_time(&mut self, now: f64) {
        if now > self.now {
            self.now = now;
        }
    }

    /// Bring geometry up to date after DOM changes (forced synchronous layout, like `offsetWidth`).
    pub fn ensure_layout(&mut self) {
        if self.needs_update() {
            let now = self.now;
            self.update(now);
        }
    }

    // ------------------------------------------------------------------------------------
    // Geometry queries
    // ------------------------------------------------------------------------------------

    /// Border-box rectangle of an element in viewport coordinates (x, y, w, h); transforms are applied
    /// to the returned bounding box.
    pub fn element_rect(&self, id: NodeId) -> Option<[f32; 4]> {
        let r = *self.rects.get(&id)?;
        // Apply the cumulative transform recorded for hit testing, if any.
        let h = self.hits.iter().find(|h| h.node == id)?;
        match h.inv.and_then(rendrix_css::animation::mat_invert) {
            Some(m) => {
                let cs = [mat_apply(m, r[0], r[1]), mat_apply(m, r[0] + r[2], r[1]), mat_apply(m, r[0], r[1] + r[3]), mat_apply(m, r[0] + r[2], r[1] + r[3])];
                let x0 = cs.iter().map(|c| c.0).fold(f32::MAX, f32::min);
                let x1 = cs.iter().map(|c| c.0).fold(f32::MIN, f32::max);
                let y0 = cs.iter().map(|c| c.1).fold(f32::MAX, f32::min);
                let y1 = cs.iter().map(|c| c.1).fold(f32::MIN, f32::max);
                Some([x0, y0, x1 - x0, y1 - y0])
            }
            None => Some(r),
        }
    }

    /// Untransformed layout size and offset within the offset parent chain (offsetWidth/Height).
    pub fn layout_size(&self, id: NodeId) -> Option<(f32, f32)> {
        self.layout_result.boxes.get(&(id as u64)).map(|b| (b.width, b.height))
    }

    /// Post-layout float/clear adjustment, applied to one containing block at a time.
    ///
    /// Taffy lays every child in-flow; this pass pins `float: left | right` boxes to the
    /// content edge and pushes `clear` boxes (and everything laid out after them) below the
    /// relevant floats. Limitations (documented in COMPATIBILITY.md): floats need an explicit
    /// width (auto-width floats size like blocks), containers do not auto-expand around
    /// floats, floats never interact across containing blocks, and inline content does not
    /// wrap around a float.
    pub(crate) fn apply_float_clear(&mut self) {
        if let Some(tree) = self.layout_tree.as_ref() {
            apply_float_clear(tree, &mut self.layout_result);
        }
    }

    /// Border-box and content-box sizes for `ResizeObserver` box options.
    /// Returns `(border_w, border_h, content_w, content_h)`; `None` when the node
    /// has no box (e.g. `display:none` or a non-element).
    pub fn box_sizes(&self, id: NodeId) -> Option<(f32, f32, f32, f32)> {
        let b = self.layout_result.boxes.get(&(id as u64))?;
        let cw = (b.width - b.padding[1] - b.padding[3] - b.border[1] - b.border[3]).max(0.0);
        let ch = (b.height - b.padding[0] - b.padding[2] - b.border[0] - b.border[2]).max(0.0);
        Some((b.width, b.height, cw, ch))
    }

    /// Topmost element under a viewport point, honouring clips, transforms, z-order and `pointer-events`.
    pub fn hit_test(&self, x: f32, y: f32) -> Option<NodeId> {
        for h in self.hits.iter().rev() {
            if !h.pointer_events {
                continue;
            }
            let c = h.clip;
            if x < c[0] || y < c[1] || x >= c[0] + c[2] || y >= c[1] + c[3] {
                continue;
            }
            let (lx, ly) = match h.inv {
                Some(m) => mat_apply(m, x, y),
                None => (x, y),
            };
            let r = h.rect;
            if lx >= r[0] && lx < r[0] + r[2] && ly >= r[1] && ly < r[1] + r[3] {
                return Some(h.node);
            }
        }
        None
    }

    pub fn cursor_at(&self, x: f32, y: f32) -> Cursor {
        let Some(mut n) = self.hit_test(x, y) else { return Cursor::Default };
        loop {
            if let Some(s) = self.style_of(n) {
                if s.cursor != Cursor::Auto {
                    return s.cursor;
                }
                if self.doc.is_text_control(n) {
                    return Cursor::Text;
                }
                if matches!(self.doc.tag(n), Some("a")) && self.doc.has_attr(n, "href") {
                    return Cursor::Pointer;
                }
            }
            match self.doc.parent(n) {
                Some(p) if self.doc.element(p).is_some() => n = p,
                _ => return Cursor::Default,
            }
        }
    }

    /// Apply a wheel delta at a point to the innermost scrollable container that can move. Returns whether anything scrolled.
    pub fn scroll_by(&mut self, x: f32, y: f32, dx: f32, dy: f32) -> bool {
        let candidates: Vec<ScrollInfo> = self.scrolls.iter().rev().copied().collect();
        // Prefer containers under the pointer, innermost (latest painted) first; the root is the fallback.
        for s in candidates {
            let inside = x >= s.clip[0] && y >= s.clip[1] && x < s.clip[0] + s.clip[2] && y < s.clip[1] + s.clip[3] && x >= s.rect[0] && y >= s.rect[1] && x < s.rect[0] + s.rect[2] && y < s.rect[1] + s.rect[3];
            if !inside {
                continue;
            }
            let (cx, cy) = self.get_scroll(s.node);
            let nx = (cx + dx).clamp(0.0, s.max_x);
            let ny = (cy + dy).clamp(0.0, s.max_y);
            if (nx, ny) != (cx, cy) {
                self.scroll.borrow_mut().insert(s.node, (nx, ny));
                self.force_repaint = true;
                return true;
            }
        }
        false
    }

    pub fn content_of(&self, id: NodeId) -> Option<Arc<InlineContent>> {
        fn find(n: &LayoutNode, id: u64) -> Option<Arc<InlineContent>> {
            if n.id == id {
                if let rendrix_layout::LayoutKind::Text(c) = &n.kind {
                    return Some(c.clone());
                }
            }
            n.children.iter().find_map(|c| find(c, id))
        }
        self.layout_tree.as_ref().and_then(|t| find(t, id as u64))
    }

    /// Content-box origin and width of a text control, for converting pointer positions to text offsets.
    pub fn text_origin(&self, id: NodeId) -> Option<(f32, f32, f32)> {
        let b = self.layout_result.boxes.get(&(id as u64))?;
        let r = self.rects.get(&id)?;
        let x = r[0] + b.border[3] + b.padding[3];
        let y = r[1] + b.border[0] + b.padding[0];
        let w = b.width - b.border[1] - b.border[3] - b.padding[1] - b.padding[3];
        Some((x, y, w))
    }

    /// Byte offset in a text control's value nearest to a viewport point.
    pub fn text_offset_at(&self, id: NodeId, x: f32, y: f32) -> Option<usize> {
        let content = self.content_of(id)?;
        let (ox, oy, w) = self.text_origin(id)?;
        let single = self.doc.tag(id) == Some("input");
        let scroll = if single { self.input_scroll.borrow().get(&id).copied().unwrap_or(0.0) } else { 0.0 };
        let off = self.text.borrow_mut().hit(&content, if single { None } else { Some(w) }, x - ox + scroll, y - oy);
        // The paragraph for an empty field is the placeholder; its offsets are not value offsets.
        Some(off.min(self.doc.value(id).len()))
    }

    // ------------------------------------------------------------------------------------
    // Style queries
    // ------------------------------------------------------------------------------------

    /// `getComputedStyle(el).getPropertyValue(name)`.
    pub fn computed_value(&self, id: NodeId, name: &str) -> String {
        let Some(s) = self.style_of(id) else { return String::new() };
        let px = |v: f32| format!("{}px", (v * 1000.0).round() / 1000.0);
        let dim = |d: rendrix_css::Dim| match d {
            rendrix_css::Dim::Auto => "auto".to_string(),
            rendrix_css::Dim::Px(v) => px(v),
            rendrix_css::Dim::Percent(p) => format!("{p}%"),
            rendrix_css::Dim::Calc(p, v) => format!("calc({p}% + {}px)", (v * 1000.0).round() / 1000.0),
        };
        let color = |c: Color| {
            let (r, g, b) = ((c.r * 255.0).round(), (c.g * 255.0).round(), (c.b * 255.0).round());
            if c.a >= 1.0 {
                format!("rgb({r}, {g}, {b})")
            } else {
                format!("rgba({r}, {g}, {b}, {})", (c.a * 1000.0).round() / 1000.0)
            }
        };
        if let Some(v) = name.strip_prefix("--") {
            return s.custom.get(&format!("--{v}")).cloned().unwrap_or_default();
        }
        // Compact 4-side shorthands like browsers (`margin: 10px` not `10px 10px 10px 10px`).
        let sides = |vals: [String; 4]| {
            let [t, r, b, l] = vals;
            if t == r && t == b && t == l {
                t
            } else if t == b && r == l {
                format!("{t} {r}")
            } else if r == l {
                format!("{t} {r} {b}")
            } else {
                format!("{t} {r} {b} {l}")
            }
        };
        let display = match s.display {
            rendrix_css::Display::None => "none",
            rendrix_css::Display::Block => "block",
            rendrix_css::Display::Inline => "inline",
            rendrix_css::Display::InlineBlock => "inline-block",
            rendrix_css::Display::Flex => "flex",
            rendrix_css::Display::InlineFlex => "inline-flex",
            rendrix_css::Display::Grid => "grid",
            rendrix_css::Display::InlineGrid => "inline-grid",
            rendrix_css::Display::Contents => "contents",
            rendrix_css::Display::Table => "table",
            rendrix_css::Display::InlineTable => "inline-table",
            rendrix_css::Display::TableCaption => "table-caption",
            rendrix_css::Display::TableColumnGroup => "table-column-group",
            rendrix_css::Display::TableColumn => "table-column",
            rendrix_css::Display::TableHeaderGroup => "table-header-group",
            rendrix_css::Display::TableRowGroup => "table-row-group",
            rendrix_css::Display::TableFooterGroup => "table-footer-group",
            rendrix_css::Display::TableRow => "table-row",
            rendrix_css::Display::TableCell => "table-cell",
            rendrix_css::Display::ListItem => "list-item",
        };
        let position = match s.position {
            rendrix_css::Position::Static => "static",
            rendrix_css::Position::Relative => "relative",
            rendrix_css::Position::Absolute => "absolute",
            rendrix_css::Position::Fixed => "fixed",
            rendrix_css::Position::Sticky => "sticky",
        };
        let overflow = |o: rendrix_css::Overflow| match o {
            rendrix_css::Overflow::Visible => "visible",
            rendrix_css::Overflow::Hidden => "hidden",
            rendrix_css::Overflow::Scroll => "scroll",
            rendrix_css::Overflow::Auto => "auto",
            rendrix_css::Overflow::Clip => "clip",
        };
        let box_sizing = match s.box_sizing {
            rendrix_css::BoxSizing::ContentBox => "content-box",
            rendrix_css::BoxSizing::BorderBox => "border-box",
        };
        let visibility = match s.visibility {
            rendrix_css::Visibility::Visible => "visible",
            rendrix_css::Visibility::Hidden => "hidden",
            rendrix_css::Visibility::Collapse => "collapse",
        };
        let cursor = match s.cursor {
            rendrix_css::Cursor::Auto => "auto",
            rendrix_css::Cursor::Default => "default",
            rendrix_css::Cursor::Pointer => "pointer",
            rendrix_css::Cursor::Text => "text",
            rendrix_css::Cursor::Move => "move",
            rendrix_css::Cursor::NotAllowed => "not-allowed",
            rendrix_css::Cursor::Grab => "grab",
            rendrix_css::Cursor::Grabbing => "grabbing",
            rendrix_css::Cursor::Crosshair => "crosshair",
            rendrix_css::Cursor::Wait => "wait",
            rendrix_css::Cursor::Help => "help",
            rendrix_css::Cursor::ColResize => "col-resize",
            rendrix_css::Cursor::RowResize => "row-resize",
        };
        let pointer_events = match s.pointer_events {
            rendrix_css::PointerEvents::Auto => "auto",
            rendrix_css::PointerEvents::None => "none",
        };
        let text_align = match s.text_align {
            rendrix_css::TextAlign::Left => "left",
            rendrix_css::TextAlign::Right => "right",
            rendrix_css::TextAlign::Center => "center",
            rendrix_css::TextAlign::Justify => "justify",
            rendrix_css::TextAlign::Start => "start",
            rendrix_css::TextAlign::End => "end",
        };
        let flex_direction = match s.flex_direction {
            rendrix_css::FlexDirection::Row => "row",
            rendrix_css::FlexDirection::RowReverse => "row-reverse",
            rendrix_css::FlexDirection::Column => "column",
            rendrix_css::FlexDirection::ColumnReverse => "column-reverse",
        };
        let flex_wrap = match s.flex_wrap {
            rendrix_css::FlexWrap::NoWrap => "nowrap",
            rendrix_css::FlexWrap::Wrap => "wrap",
            rendrix_css::FlexWrap::WrapReverse => "wrap-reverse",
        };
        let align = |a: rendrix_css::Align| match a {
            rendrix_css::Align::Auto => "auto",
            rendrix_css::Align::Normal => "normal",
            rendrix_css::Align::Start => "start",
            rendrix_css::Align::End => "end",
            rendrix_css::Align::FlexStart => "flex-start",
            rendrix_css::Align::FlexEnd => "flex-end",
            rendrix_css::Align::Center => "center",
            rendrix_css::Align::Stretch => "stretch",
            rendrix_css::Align::Baseline => "baseline",
            rendrix_css::Align::SpaceBetween => "space-between",
            rendrix_css::Align::SpaceAround => "space-around",
            rendrix_css::Align::SpaceEvenly => "space-evenly",
        };
        let font_style = match s.font_style {
            rendrix_css::FontStyle::Normal => "normal",
            rendrix_css::FontStyle::Italic => "italic",
            rendrix_css::FontStyle::Oblique => "oblique",
        };
        let text_transform = match s.text_transform {
            rendrix_css::TextTransform::None => "none",
            rendrix_css::TextTransform::Uppercase => "uppercase",
            rendrix_css::TextTransform::Lowercase => "lowercase",
            rendrix_css::TextTransform::Capitalize => "capitalize",
        };
        let white_space = match s.white_space {
            rendrix_css::WhiteSpace::Normal => "normal",
            rendrix_css::WhiteSpace::NoWrap => "nowrap",
            rendrix_css::WhiteSpace::Pre => "pre",
            rendrix_css::WhiteSpace::PreWrap => "pre-wrap",
            rendrix_css::WhiteSpace::PreLine => "pre-line",
        };
        let border_style = |b: rendrix_css::BorderStyle| match b {
            rendrix_css::BorderStyle::None => "none",
            rendrix_css::BorderStyle::Hidden => "hidden",
            rendrix_css::BorderStyle::Solid => "solid",
            rendrix_css::BorderStyle::Dashed => "dashed",
            rendrix_css::BorderStyle::Dotted => "dotted",
            rendrix_css::BorderStyle::Double => "double",
        };
        let object_fit = match s.object_fit {
            rendrix_css::ObjectFit::Fill => "fill",
            rendrix_css::ObjectFit::Contain => "contain",
            rendrix_css::ObjectFit::Cover => "cover",
            rendrix_css::ObjectFit::None => "none",
            rendrix_css::ObjectFit::ScaleDown => "scale-down",
        };
        let user_select = match s.user_select {
            rendrix_css::UserSelect::Auto => "auto",
            rendrix_css::UserSelect::None => "none",
            rendrix_css::UserSelect::Text => "text",
            rendrix_css::UserSelect::All => "all",
        };
        let line_height = match s.line_height {
            rendrix_css::LineHeight::Normal => "normal".to_string(),
            rendrix_css::LineHeight::Number(n) => px(n * s.font_size),
            rendrix_css::LineHeight::Px(p) => px(p),
        };
        let decoration_line = {
            let mut parts = Vec::new();
            if s.text_decoration.underline {
                parts.push("underline");
            }
            if s.text_decoration.overline {
                parts.push("overline");
            }
            if s.text_decoration.line_through {
                parts.push("line-through");
            }
            if parts.is_empty() { "none".to_string() } else { parts.join(" ") }
        };
        let box_size = self.layout_result.boxes.get(&(id as u64));
        match name {
            "display" => display.into(),
            "position" => position.into(),
            "float" => match s.float {
                rendrix_css::Float::None => "none",
                rendrix_css::Float::Left => "left",
                rendrix_css::Float::Right => "right",
            }
            .into(),
            "clear" => match s.clear {
                rendrix_css::Clear::None => "none",
                rendrix_css::Clear::Left => "left",
                rendrix_css::Clear::Right => "right",
                rendrix_css::Clear::Both => "both",
            }
            .into(),
            "column-count" => s.column_count.map(|n| n.to_string()).unwrap_or_else(|| "auto".into()),
            "column-width" => dim(s.column_width),
            "column-span" => {
                if s.column_span_all {
                    "all".into()
                } else {
                    "none".into()
                }
            }
            "columns" => {
                let w = if matches!(s.column_width, rendrix_css::Dim::Auto) { "auto".to_string() } else { dim(s.column_width) };
                let n = s.column_count.map(|n| n.to_string()).unwrap_or_else(|| "auto".to_string());
                if w == "auto" && n == "auto" {
                    "auto".into()
                } else if w == "auto" {
                    n
                } else if n == "auto" {
                    w
                } else {
                    format!("{w} {n}")
                }
            }
            "box-sizing" => box_sizing.into(),
            "color" => color(s.color),
            "background-color" => color(s.background_color),
            "background" => color(s.background_color),
            "background-image" => {
                if s.background_images.is_empty() {
                    "none".into()
                } else {
                    s.background_images
                        .iter()
                        .map(|img| match img {
                            rendrix_css::BackgroundImage::Url(u) => format!("url(\"{u}\")"),
                            rendrix_css::BackgroundImage::LinearGradient { angle, stops } => {
                                let deg = angle * 180.0 / std::f32::consts::PI;
                                let stops = stops
                                    .iter()
                                    .map(|st| {
                                        let c = color(st.color);
                                        match st.pos {
                                            Some(p) => format!("{c} {}%", (p * 100.0).round()),
                                            None => c,
                                        }
                                    })
                                    .collect::<Vec<_>>()
                                    .join(", ");
                                format!("linear-gradient({deg}deg, {stops})")
                            }
                            rendrix_css::BackgroundImage::RadialGradient { stops } => {
                                let stops = stops
                                    .iter()
                                    .map(|st| {
                                        let c = color(st.color);
                                        match st.pos {
                                            Some(p) => format!("{c} {}%", (p * 100.0).round()),
                                            None => c,
                                        }
                                    })
                                    .collect::<Vec<_>>()
                                    .join(", ");
                                format!("radial-gradient({stops})")
                            }
                        })
                        .collect::<Vec<_>>()
                        .join(", ")
                }
            }
            "background-repeat" => match s.background_repeat {
                rendrix_css::BackgroundRepeat::Repeat => "repeat",
                rendrix_css::BackgroundRepeat::NoRepeat => "no-repeat",
                rendrix_css::BackgroundRepeat::RepeatX => "repeat-x",
                rendrix_css::BackgroundRepeat::RepeatY => "repeat-y",
            }
            .into(),
            "box-shadow" => {
                if s.box_shadow.is_empty() {
                    "none".into()
                } else {
                    s.box_shadow
                        .iter()
                        .map(|sh| {
                            let mut v = format!("{} {} {} {} {}", px(sh.dx), px(sh.dy), px(sh.blur), px(sh.spread), color(sh.color));
                            if sh.inset {
                                v.push_str(" inset");
                            }
                            v
                        })
                        .collect::<Vec<_>>()
                        .join(", ")
                }
            }
            "opacity" => format!("{}", s.opacity),
            "visibility" => visibility.into(),
            "font-size" => px(s.font_size),
            "font-weight" => s.font_weight.to_string(),
            "font-family" => s.font_family.join(", "),
            "font-style" => font_style.into(),
            "line-height" => line_height.clone(),
            "letter-spacing" => px(s.letter_spacing),
            "word-spacing" => px(s.word_spacing),
            "font" => {
                let size_line = format!("{}/{}", px(s.font_size), line_height);
                if font_style == "normal" {
                    format!("{} {} {}", s.font_weight, size_line, s.font_family.join(", "))
                } else {
                    format!("{font_style} {} {} {}", s.font_weight, size_line, s.font_family.join(", "))
                }
            }
            "text-align" => text_align.into(),
            "text-decoration-line" => decoration_line.clone(),
            "text-decoration-style" => "solid".into(),
            "text-decoration-color" => color(s.color),
            "text-decoration" => decoration_line.clone(),
            "text-transform" => text_transform.into(),
            "white-space" => white_space.into(),
            "text-overflow" => {
                if s.text_overflow_ellipsis { "ellipsis".into() } else { "clip".into() }
            }
            "text-shadow" => {
                if s.text_shadow.is_empty() {
                    "none".into()
                } else {
                    s.text_shadow
                        .iter()
                        .map(|sh| format!("{} {} {} {}", px(sh.dx), px(sh.dy), px(sh.blur), color(sh.color)))
                        .collect::<Vec<_>>()
                        .join(", ")
                }
            }
            "filter" => serialize_filter(&s.filter),
            "backdrop-filter" | "-webkit-backdrop-filter" => serialize_filter(&s.backdrop_filter),
            "clip-path" => {
                use rendrix_css::ClipPath as CP;
                match &s.clip_path {
                    CP::None => "none".into(),
                    CP::Inset { top, right, bottom, left } => format!("inset({}, {}, {}, {})", dim(*top), dim(*right), dim(*bottom), dim(*left)),
                    CP::Circle { r, cx, cy } => format!("circle({} at {} {})", dim(*r), dim(*cx), dim(*cy)),
                    CP::Ellipse { rx, ry, cx, cy } => format!("ellipse({} {} at {} {})", dim(*rx), dim(*ry), dim(*cx), dim(*cy)),
                    CP::Polygon(pts) => format!("polygon({})", pts.iter().map(|(x, y)| format!("{} {}", dim(*x), dim(*y))).collect::<Vec<_>>().join(", ")),
                }
            }
            "mask-image" | "-webkit-mask-image" | "mask" => s.mask_image.as_deref().map(|u| format!("url(\"{u}\")")).unwrap_or_else(|| "none".into()),
            "caret-color" => s.caret_color.map(color).unwrap_or_else(|| "auto".into()),
            "user-select" => user_select.into(),
            "touch-action" => match s.touch_action { 7 => "auto".to_string(), 0 => "none".to_string(), m => [(1u8, "pan-x"), (2, "pan-y"), (4, "pinch-zoom")].iter().filter(|(b, _)| m & b != 0).map(|(_, n)| *n).collect::<Vec<_>>().join(" ") },
            "list-style-type" => {
                if s.list_style_none { "none".into() } else { "disc".into() }
            }
            "width" => box_size.map(|b| px(b.width - if s.box_sizing == rendrix_css::BoxSizing::ContentBox { b.padding[1] + b.padding[3] + b.border[1] + b.border[3] } else { 0.0 })).unwrap_or_else(|| dim(s.width)),
            "height" => box_size.map(|b| px(b.height - if s.box_sizing == rendrix_css::BoxSizing::ContentBox { b.padding[0] + b.padding[2] + b.border[0] + b.border[2] } else { 0.0 })).unwrap_or_else(|| dim(s.height)),
            "min-width" => dim(s.min_width),
            "min-height" => dim(s.min_height),
            "max-width" => dim(s.max_width),
            "max-height" => dim(s.max_height),
            "aspect-ratio" => s.aspect_ratio.map(|v| format!("{v}")).unwrap_or_else(|| "auto".into()),
            "margin" => sides([dim(s.margin[0]), dim(s.margin[1]), dim(s.margin[2]), dim(s.margin[3])]),
            "margin-top" => dim(s.margin[0]),
            "margin-right" => dim(s.margin[1]),
            "margin-bottom" => dim(s.margin[2]),
            "margin-left" => dim(s.margin[3]),
            "padding" => sides([dim(s.padding[0]), dim(s.padding[1]), dim(s.padding[2]), dim(s.padding[3])]),
            "padding-top" => dim(s.padding[0]),
            "padding-right" => dim(s.padding[1]),
            "padding-bottom" => dim(s.padding[2]),
            "padding-left" => dim(s.padding[3]),
            "inset" => sides([dim(s.inset[0]), dim(s.inset[1]), dim(s.inset[2]), dim(s.inset[3])]),
            "top" => dim(s.inset[0]),
            "right" => dim(s.inset[1]),
            "bottom" => dim(s.inset[2]),
            "left" => dim(s.inset[3]),
            "border-width" => sides([px(s.border_width[0]), px(s.border_width[1]), px(s.border_width[2]), px(s.border_width[3])]),
            "border-top-width" => px(s.border_width[0]),
            "border-right-width" => px(s.border_width[1]),
            "border-bottom-width" => px(s.border_width[2]),
            "border-left-width" => px(s.border_width[3]),
            "border-style" => sides([
                border_style(s.border_style[0]).to_string(),
                border_style(s.border_style[1]).to_string(),
                border_style(s.border_style[2]).to_string(),
                border_style(s.border_style[3]).to_string(),
            ]),
            "border-top-style" => border_style(s.border_style[0]).into(),
            "border-right-style" => border_style(s.border_style[1]).into(),
            "border-bottom-style" => border_style(s.border_style[2]).into(),
            "border-left-style" => border_style(s.border_style[3]).into(),
            "border-color" => sides([color(s.border_color[0]), color(s.border_color[1]), color(s.border_color[2]), color(s.border_color[3])]),
            "border-top-color" => color(s.border_color[0]),
            "border-right-color" => color(s.border_color[1]),
            "border-bottom-color" => color(s.border_color[2]),
            "border-left-color" => color(s.border_color[3]),
            "border" => format!(
                "{} {} {}",
                sides([px(s.border_width[0]), px(s.border_width[1]), px(s.border_width[2]), px(s.border_width[3])]),
                sides([
                    border_style(s.border_style[0]).to_string(),
                    border_style(s.border_style[1]).to_string(),
                    border_style(s.border_style[2]).to_string(),
                    border_style(s.border_style[3]).to_string(),
                ]),
                sides([color(s.border_color[0]), color(s.border_color[1]), color(s.border_color[2]), color(s.border_color[3])]),
            ),
            "border-radius" => sides([dim(s.border_radius[0]), dim(s.border_radius[1]), dim(s.border_radius[2]), dim(s.border_radius[3])]),
            "border-top-left-radius" => dim(s.border_radius[0]),
            "border-top-right-radius" => dim(s.border_radius[1]),
            "border-bottom-right-radius" => dim(s.border_radius[2]),
            "border-bottom-left-radius" => dim(s.border_radius[3]),
            "outline-width" => px(s.outline_width),
            "outline-style" => {
                if s.outline_width <= 0.0 { "none".into() } else { "solid".into() }
            }
            "outline-color" => color(s.outline_color),
            "outline-offset" => px(s.outline_offset),
            "outline" => format!(
                "{} {} {}",
                px(s.outline_width),
                if s.outline_width <= 0.0 { "none" } else { "solid" },
                color(s.outline_color),
            ),
            "z-index" => s.z_index.map(|z| z.to_string()).unwrap_or_else(|| "auto".into()),
            "overflow" => {
                let x = overflow(s.overflow_x);
                let y = overflow(s.overflow_y);
                if x == y { x.into() } else { format!("{x} {y}") }
            },
            "overflow-x" => overflow(s.overflow_x).into(),
            "overflow-y" => overflow(s.overflow_y).into(),
            "cursor" => cursor.into(),
            "pointer-events" => pointer_events.into(),
            "flex-direction" => flex_direction.into(),
            "flex-wrap" => flex_wrap.into(),
            "flex-flow" => format!("{flex_direction} {flex_wrap}"),
            "flex-grow" => format!("{}", s.flex_grow),
            "flex-shrink" => format!("{}", s.flex_shrink),
            "flex-basis" => dim(s.flex_basis),
            "flex" => format!("{} {} {}", s.flex_grow, s.flex_shrink, dim(s.flex_basis)),
            "order" => format!("{}", s.order),
            "align-items" => align(s.align_items).into(),
            "align-self" => align(s.align_self).into(),
            "align-content" => align(s.align_content).into(),
            "justify-content" => align(s.justify_content).into(),
            "justify-items" => align(s.justify_items).into(),
            "justify-self" => align(s.justify_self).into(),
            "row-gap" => dim(s.row_gap),
            "column-gap" => dim(s.column_gap),
            "gap" => {
                let r = dim(s.row_gap);
                let c = dim(s.column_gap);
                if r == c { r } else { format!("{r} {c}") }
            },
            "object-fit" => object_fit.into(),
            "content" => s.content.clone().map(|v| format!("\"{v}\"")).unwrap_or_else(|| "normal".into()),
            "transform" => {
                if s.transform.is_empty() {
                    "none".into()
                } else {
                    let m = rendrix_css::animation::transform_matrix(&s.transform, s.transform_origin, 0.0, 0.0);
                    format!("matrix({}, {}, {}, {}, {}, {})", m[0], m[1], m[2], m[3], m[4], m[5])
                }
            }
            "transform-origin" => format!("{} {}", dim(s.transform_origin.0), dim(s.transform_origin.1)),
            _ => String::new(),
        }
    }
}

/// One containing block of the float/clear post-pass; see
/// [`Engine::apply_float_clear`].
fn apply_float_clear(tree: &LayoutNode, out: &mut LayoutResult) {
    use rendrix_css::{Clear, Float};

    fn adjust(parent: &LayoutNode, out: &mut LayoutResult) {
        if let Some(pbox) = out.boxes.get(&parent.id).copied() {
            let cx = pbox.border[3] + pbox.padding[3];
            let cw = (pbox.width - pbox.border[1] - pbox.border[3] - pbox.padding[1] - pbox.padding[3]).max(0.0);
            let mut kids: Vec<(u64, f32, f32)> = parent
                .children
                .iter()
                .filter(|c| out.boxes.contains_key(&c.id))
                .map(|c| {
                    let b = &out.boxes[&c.id];
                    (c.id, b.y, b.x)
                })
                .collect();
            kids.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap().then(a.2.partial_cmp(&b.2).unwrap()));
            let mut push = 0.0f32;
            let (mut left_bottom, mut right_bottom) = (f32::NEG_INFINITY, f32::NEG_INFINITY);
            for (id, _, _) in kids {
                let Some(st) = parent.children.iter().find(|c| c.id == id).map(|c| c.style.clone()) else { continue };
                let Some(b) = out.boxes.get_mut(&id) else { continue };
                b.y += push;
                let req = match st.clear {
                    Clear::Left => left_bottom,
                    Clear::Right => right_bottom,
                    Clear::Both => left_bottom.max(right_bottom),
                    Clear::None => f32::NEG_INFINITY,
                };
                if b.y < req {
                    let dy = req - b.y;
                    b.y += dy;
                    push += dy;
                }
                match st.float {
                    Float::Left => {
                        b.x = cx;
                        left_bottom = left_bottom.max(b.y + b.height);
                    }
                    Float::Right => {
                        b.x = cx + cw - b.width;
                        right_bottom = right_bottom.max(b.y + b.height);
                    }
                    Float::None => {}
                }
            }
        }
        for c in &parent.children {
            adjust(c, out);
        }
    }

    adjust(tree, out);
}

fn serialize_filter(ops: &[rendrix_css::FilterOp]) -> String {
    if ops.is_empty() {
        return "none".into();
    }
    ops.iter()
        .map(|op| match op {
            rendrix_css::FilterOp::Blur(r) => format!("blur({}px)", (r * 1000.0).round() / 1000.0),
            rendrix_css::FilterOp::Brightness(b) => format!("brightness({b})"),
            rendrix_css::FilterOp::Grayscale(g) => format!("grayscale({g})"),
        })
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests;
