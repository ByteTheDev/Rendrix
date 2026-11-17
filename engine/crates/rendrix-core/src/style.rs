//! Style resolution for the DOM: stylesheet collection, restyling, transitions and keyframe animations.

use crate::Engine;
use rendrix_css::animation::{interpolate_property, property_differs, sample_animation, ANIMATABLE};
use rendrix_css::parser::{MediaQueryList, Rule};
use rendrix_css::{compute_style, ComputedStyle, ContainerType, MediaContext, Origin, PseudoElement, RuleSet, StyleContext, TimingFunction};
use rendrix_dom::{ElRef, NodeId, NodeKind};
use std::sync::Arc;

pub struct ActiveTransition {
    pub prop: &'static str,
    /// The effective (possibly mid-transition) style when the transition started.
    pub from: Arc<ComputedStyle>,
    pub start: f64,
    pub duration: f32,
    pub delay: f32,
    pub timing: TimingFunction,
}

pub struct ActiveAnimation {
    pub name: String,
    pub spec_index: usize,
    pub start: f64,
}

#[derive(Default)]
pub struct AnimState {
    pub transitions: Vec<ActiveTransition>,
    pub animations: Vec<ActiveAnimation>,
}

impl Engine {
    pub fn media_context(&self) -> MediaContext {
        MediaContext { width: self.viewport.0, height: self.viewport.1, dpr: self.scale, dark: self.dark, reduced_motion: false }
    }

    /// Rebuild the map of container sizes from the most recent layout. Nested
    /// containers overwrite their ancestors, so descendants see the nearest one.
    pub fn refresh_container_sizes(&mut self) {
        use rendrix_layout::{BoxLayout, LayoutNode};
        let mut map = std::collections::HashMap::new();
        fn fill(n: &LayoutNode, map: &mut std::collections::HashMap<u64, (String, f32, f32)>, name: &str, size: (f32, f32)) {
            map.insert(n.id, (name.to_string(), size.0, size.1));
            for c in &n.children {
                fill(c, map, name, size);
            }
        }
        fn walk(
            n: &LayoutNode,
            boxes: &std::collections::HashMap<u64, BoxLayout>,
            map: &mut std::collections::HashMap<u64, (String, f32, f32)>,
        ) {
            if !matches!(n.style.container_type, ContainerType::Normal) {
                if let Some(b) = boxes.get(&n.id) {
                    let name = n.style.container_name.as_deref().unwrap_or("");
                    fill(n, map, name, (b.width, b.height));
                }
            }
            for c in &n.children {
                walk(c, boxes, map);
            }
        }
        if let Some(tree) = &self.layout_tree {
            walk(tree, &self.layout_result.boxes, &mut map);
        }
        self.container_sizes = Arc::new(map);
    }

    pub fn style_context(&self) -> StyleContext {
        StyleContext {
            viewport_w: self.viewport.0,
            viewport_h: self.viewport.1,
            root_font_size: 16.0,
            safe_area: self.safe_area,
            keyboard_h: self.keyboard_h,
            container_sizes: self.container_sizes.clone(),
        }
    }

    /// Re-collect `<style>` and `<link rel=stylesheet>` sheets when they or the media context changed.
    /// Returns true when the rule set was rebuilt (all styles are then stale).
    ///
    /// P2: stylesheets are parsed once per content hash (`parse_cached`; UA sheet once via
    /// `ua_sheet`). A rebuild caused only by a media-context change re-filters cached rules
    /// instead of re-tokenising anything.
    pub(crate) fn refresh_rules(&mut self) -> bool {
        let media = self.media_context();
        let media_sig = (media.width.to_bits(), media.height.to_bits(), media.dpr.to_bits(), media.dark);
        if !self.doc.sheets_dirty && !self.rules_dirty && self.rules_media_sig == Some(media_sig) {
            return false;
        }
        self.doc.sheets_dirty = false;
        self.rules_dirty = false;
        self.rules_media_sig = Some(media_sig);
        let mut rules = RuleSet::new();
        rules.add_stylesheet(&rendrix_css::parser::ua_sheet(), Origin::UserAgent, &media);
        let root = self.doc.root();
        // `@import` resolution against bundled assets (cycle-guarded, depth-capped).
        // `base` scopes relative URLs (e.g. the `href` of a `<link>` sheet).
        fn add_css(engine: &mut Engine, rules: &mut RuleSet, css: &str, media: &MediaContext, base: Option<&str>, depth: usize, visited: &mut std::collections::HashSet<String>) {
            if depth > 8 {
                return;
            }
            let sheet = rendrix_css::parser::parse_cached(css);
            // Partition: imports splice in place (recursively), everything else indexes.
            let mut rest = Vec::new();
            for rule in &sheet.rules {
                if let Rule::Import(url, mq) = rule {
                    if !mq.trim().is_empty() && mq.to_ascii_lowercase() != "all" {
                        // Gate on the import's media query (best-effort; unknown media = apply).
                        let list = MediaQueryList::parse(mq);
                        // `matches` needs a full context; reuse the current one.
                        if !list.matches(media) {
                            continue;
                        }
                    }
                    let key = resolve_import_key(url, base);
                    if !visited.insert(key.clone()) {
                        continue;
                    }
                    if let Some(bytes) = engine.assets.read(&key) {
                        let text = String::from_utf8_lossy(&bytes).into_owned();
                        let nested_base = base_dir(&key);
                        add_css(engine, rules, &text, media, nested_base.as_deref(), depth + 1, visited);
                    } else if let Some(bytes) = engine.assets.read(url) {
                        let text = String::from_utf8_lossy(&bytes).into_owned();
                        add_css(engine, rules, &text, media, base, depth + 1, visited);
                    }
                } else {
                    rest.push(rule.clone());
                }
            }
            if !rest.is_empty() {
                let tmp = rendrix_css::parser::Stylesheet { rules: rest };
                rules.add_stylesheet(&tmp, Origin::Author, media);
            }
        }
        fn base_dir(path: &str) -> Option<String> {
            match path.rfind('/') {
                Some(i) => Some(path[..i].to_string()),
                None => None,
            }
        }
        fn resolve_import_key(url: &str, base: Option<&str>) -> String {
            let mut u = url.trim().to_string();
            // Strip query/fragment for bundled-asset lookup.
            if let Some(i) = u.find(['?', '#']) {
                u.truncate(i);
            }
            while u.starts_with("./") {
                u = u[2..].to_string();
            }
            if u.starts_with('/') {
                u = u[1..].to_string();
            }
            if u.contains("://") || u.starts_with("data:") {
                return u;
            }
            if let Some(b) = base {
                if !b.is_empty() {
                    return format!("{}/{}", b.trim_end_matches('/'), u);
                }
            }
            u
        }
        for n in self.doc.depth_first(root) {
            match self.doc.tag(n) {
                Some("style") => {
                    let css = self.doc.text_content(n);
                    let mut visited = std::collections::HashSet::new();
                    // Borrow-split: `add_css` needs `&mut Engine` but we hold
                    // `&mut self` already — call inline via a local reborrow.
                    // (Nested function takes `&mut Engine`; `self` coerces.)
                    add_css(&mut *self, &mut rules, &css, &media, None, 0, &mut visited);
                }
                Some("link") => {
                    let is_sheet = self.doc.get_attr(n, "rel").is_some_and(|r| r.eq_ignore_ascii_case("stylesheet"));
                    if is_sheet {
                        if let Some(href) = self.doc.get_attr(n, "href").map(|h| h.to_string()) {
                            if let Some(bytes) = self.assets.read(&href) {
                                let css = String::from_utf8_lossy(&bytes).into_owned();
                                let mut visited = std::collections::HashSet::new();
                                visited.insert(href.clone());
                                let base = base_dir(&href);
                                add_css(&mut *self, &mut rules, &css, &media, base.as_deref(), 0, &mut visited);
                            }
                        }
                    }
                }
                _ => {}
            }
        }
        for sheet in self.adopted_sheets.clone() {
            let mut visited = std::collections::HashSet::new();
            add_css(&mut *self, &mut rules, &sheet, &media, None, 0, &mut visited);
        }
        // Register @font-face fonts that point at bundled assets.
        let faces = rules.font_faces.clone();
        for f in faces {
            if !self.loaded_fonts.insert(f.src.clone()) {
                continue;
            }
            if let Some(url) = extract_url(&f.src) {
                if let Some(bytes) = self.assets.read(&url) {
                    self.text.borrow_mut().load_font(bytes);
                }
            }
        }
        self.rules = rules;
        true
    }

    /// Recompute styles for dirty subtrees (or everything).
    /// Returns whether any computed style actually changed.
    pub(crate) fn restyle(&mut self, all: bool, roots: Vec<NodeId>, state_roots: Vec<NodeId>) -> bool {
        self.style_changed = all;
        self.style_layout_changed = false;
        let sc = self.style_context();
        let cap = self.doc_capacity();
        if self.styles.len() < cap {
            self.styles.resize(cap + 64, None);
        }
        let start_nodes: Vec<NodeId> = if all {
            vec![self.doc.root()]
        } else {
            let mut v = roots;
            v.retain(|n| self.doc.is_alive(*n) && self.doc.is_connected(*n));
            // Drop roots nested inside other dirty roots.
            let set: std::collections::HashSet<NodeId> = v.iter().copied().collect();
            v.into_iter()
                .filter(|n| {
                    let mut p = self.doc.parent(*n);
                    while let Some(q) = p {
                        if set.contains(&q) {
                            return false;
                        }
                        p = self.doc.parent(q);
                    }
                    true
                })
                .collect()
        };
        // P5: the covered set is only consulted for state roots; skip the O(n) scan when
        // there is nothing to check it against.
        let covered: Option<std::collections::HashSet<NodeId>> = if !all && !state_roots.is_empty() {
            Some(start_nodes.iter().flat_map(|n| self.doc.depth_first(*n)).collect())
        } else {
            None
        };
        for start in start_nodes {
            let parent_style = self.doc.parent(start).and_then(|p| self.styles.get(p as usize).cloned().flatten());
            self.restyle_subtree(start, parent_style, &sc, false);
        }
        // Interaction-state changes: restyle the element itself; descend only where something changed.
        if !all {
            for n in state_roots {
                if covered.as_ref().is_some_and(|c| c.contains(&n)) || !self.doc.is_alive(n) || self.doc.element(n).is_none() || !self.doc.is_connected(n) {
                    continue;
                }
                let parent_style = self.doc.parent(n).and_then(|p| self.styles.get(p as usize).cloned().flatten());
                self.restyle_subtree(n, parent_style, &sc, !(self.rules.state_deps_nonlocal || self.rules.structural_deps));
            }
        }
        self.style_changed
    }

    fn restyle_subtree(&mut self, id: NodeId, parent_style: Option<Arc<ComputedStyle>>, sc: &StyleContext, shallow: bool) {
        match &self.doc.node(id).kind {
            NodeKind::Document => {
                let kids: Vec<NodeId> = self.doc.children(id).collect();
                for k in kids {
                    self.restyle_subtree(k, None, sc, shallow);
                }
            }
            NodeKind::Element(_) => {
                let new = {
                    let el = ElRef::new(&self.doc, id);
                    let inline = self.doc.element(id).unwrap().inline_style.clone();
                    Arc::new(compute_style(&el, &self.rules, &inline, parent_style.as_deref(), sc, None))
                };
                for (which, key) in [(PseudoElement::Before, true), (PseudoElement::After, false)] {
                    let fresh = self.pseudo_style(id, which, &new, sc);
                    let prev = self.pseudo.get(&(id, key)).cloned();
                    if fresh.as_deref() != prev.as_deref() {
                        self.style_changed = true;
                        // Generated boxes affect layout; be conservative.
                        self.style_layout_changed = true;
                    }
                    match fresh {
                        Some(s) => {
                            self.pseudo.insert((id, key), s);
                        }
                        None => {
                            self.pseudo.remove(&(id, key));
                        }
                    }
                }
                let old = self.styles[id as usize].take();
                let unchanged = old.as_ref().is_some_and(|o| **o == *new);
                if !unchanged {
                    self.style_changed = true;
                    // New boxes always need layout; otherwise only when a
                    // layout-affecting property differs (paint-only changes skip Taffy).
                    let layout_diff = match old.as_ref() {
                        None => true,
                        Some(o) => !o.layout_eq(&new),
                    };
                    if layout_diff {
                        self.style_layout_changed = true;
                    }
                }
                self.update_motion(id, old.as_ref(), &new);
                // Keep the old Arc when nothing changed so cached comparisons stay cheap.
                let kept = if unchanged { old.unwrap() } else { new };
                self.styles[id as usize] = Some(kept.clone());
                let kids: Vec<NodeId> = self.doc.children(id).collect();
                for k in kids {
                    if shallow && unchanged && self.styles.get(k as usize).is_some_and(|s| s.is_some()) {
                        continue;
                    }
                    self.restyle_subtree(k, Some(kept.clone()), sc, shallow);
                }
            }
            _ => {}
        }
    }

    fn pseudo_style(&self, id: NodeId, which: PseudoElement, parent: &Arc<ComputedStyle>, sc: &StyleContext) -> Option<Arc<ComputedStyle>> {
        let el = ElRef::new(&self.doc, id);
        let s = compute_style(&el, &self.rules, &[], Some(parent), sc, Some(which));
        s.content.is_some().then(|| Arc::new(s))
    }

    /// Start transitions / animations implied by the change from `old` to `new`.
    fn update_motion(&mut self, id: NodeId, old: Option<&Arc<ComputedStyle>>, new: &Arc<ComputedStyle>) {
        let has_any = !new.transitions.is_empty() || !new.animations.is_empty() || self.anim.contains_key(&id);
        if !has_any {
            return;
        }
        let now = self.now;
        // Effective style before this change, so interrupted transitions restart from where they were.
        let before_effective = old.map(|o| self.effective_from(id, o, now));
        let has_keyframes: Vec<bool> = new.animations.iter().map(|s| self.rules.keyframes.contains_key(&s.name)).collect();
        let state = self.anim.entry(id).or_default();

        if let (Some(old), Some(before)) = (old, before_effective) {
            for t in &new.transitions {
                if t.duration <= 0.0 && t.delay <= 0.0 {
                    continue;
                }
                for prop in ANIMATABLE {
                    if t.property != "all" && t.property != *prop {
                        continue;
                    }
                    if property_differs(prop, old, new) {
                        state.transitions.retain(|x| x.prop != *prop);
                        state.transitions.push(ActiveTransition { prop, from: before.clone(), start: now, duration: t.duration, delay: t.delay, timing: t.timing.clone() });
                    }
                }
            }
        }
        // Keyframe animations: start new names, drop removed ones.
        state.animations.retain(|a| new.animations.iter().any(|s| s.name == a.name));
        for (i, spec) in new.animations.iter().enumerate() {
            if !state.animations.iter().any(|a| a.name == spec.name) && has_keyframes[i] {
                state.animations.push(ActiveAnimation { name: spec.name.clone(), spec_index: i, start: now });
            }
        }
        for a in state.animations.iter_mut() {
            if let Some(i) = new.animations.iter().position(|s| s.name == a.name) {
                a.spec_index = i;
            }
        }
        if state.transitions.is_empty() && state.animations.is_empty() {
            self.anim.remove(&id);
        }
    }

    /// The style as displayed at time `now` (base style plus transitions and animations).
    pub(crate) fn effective_from(&self, id: NodeId, base: &Arc<ComputedStyle>, now: f64) -> Arc<ComputedStyle> {
        let Some(state) = self.anim.get(&id) else { return base.clone() };
        let mut out = (**base).clone();
        for t in &state.transitions {
            let elapsed = (now - t.start) as f32 - t.delay;
            if elapsed < 0.0 {
                interpolate_property(t.prop, &t.from, base, 0.0, &mut out);
                continue;
            }
            let p = if t.duration <= 0.0 { 1.0 } else { (elapsed / t.duration).clamp(0.0, 1.0) };
            if p < 1.0 {
                interpolate_property(t.prop, &t.from, base, t.timing.eval(p), &mut out);
            }
        }
        let parent_style = self.doc.parent(id).and_then(|p| self.styles.get(p as usize).cloned().flatten());
        let default_parent = ComputedStyle::default();
        let sc = self.style_context();
        for a in &state.animations {
            let (Some(kf), Some(spec)) = (self.rules.keyframes.get(&a.name), base.animations.get(a.spec_index)) else { continue };
            let elapsed = (now - a.start) as f32;
            if let Some(sample) = sample_animation(&out, parent_style.as_deref().unwrap_or(&default_parent), kf, spec, elapsed, &sc) {
                out = sample.style;
            }
        }
        Arc::new(out)
    }

    /// Drop finished motion and report whether anything is still moving.
    pub(crate) fn tick_motion(&mut self, now: f64) -> bool {
        let mut done_nodes = Vec::new();
        let mut any = false;
        for (id, st) in self.anim.iter_mut() {
            st.transitions.retain(|t| (now - t.start) as f32 - t.delay < t.duration);
            let base = self.styles.get(*id as usize).cloned().flatten();
            st.animations.retain(|a| {
                let Some(spec) = base.as_ref().and_then(|b| b.animations.get(a.spec_index)) else { return false };
                let finished = !spec.iterations.is_infinite() && spec.duration > 0.0 && (now - a.start) as f32 - spec.delay >= spec.duration * spec.iterations;
                !finished || matches!(spec.fill, rendrix_css::FillMode::Forwards | rendrix_css::FillMode::Both)
            });
            let running_anim = st.animations.iter().any(|a| {
                base.as_ref().and_then(|b| b.animations.get(a.spec_index)).is_some_and(|s| s.iterations.is_infinite() || (now - a.start) as f32 - s.delay < s.duration * s.iterations)
            });
            if !st.transitions.is_empty() || running_anim {
                any = true;
            }
            if st.transitions.is_empty() && st.animations.is_empty() {
                done_nodes.push(*id);
            }
        }
        for id in done_nodes {
            self.anim.remove(&id);
        }
        any
    }

    /// True when any currently-running transition or keyframe animation can
    /// change box geometry. Paint-only motion (opacity, colours, transforms, …)
    /// returns false so frames repaint without a Taffy re-solve.
    pub(crate) fn anim_needs_layout(&self) -> bool {
        for (id, st) in &self.anim {
            for t in &st.transitions {
                if ComputedStyle::is_layout_prop(t.prop) {
                    // `all` is conservative: check whether the endpoints actually
                    // differ in a layout-affecting property.
                    if t.prop != "all" {
                        return true;
                    }
                    if let Some(base) = self.styles.get(*id as usize).and_then(|s| s.as_ref()) {
                        if !t.from.layout_eq(base) {
                            return true;
                        }
                    } else {
                        return true;
                    }
                }
            }
            for a in &st.animations {
                // Only running animations force per-frame layout; retained
                // fill-forwards finales are static (their geometry is already built).
                let running = self
                    .styles
                    .get(*id as usize)
                    .and_then(|s| s.as_ref())
                    .and_then(|b| b.animations.get(a.spec_index))
                    .is_some_and(|s| {
                        s.iterations.is_infinite()
                            || (self.now - a.start) as f32 - s.delay < s.duration * s.iterations
                    });
                if !running {
                    continue;
                }
                let Some(kf) = self.rules.keyframes.get(&a.name) else { continue };
                // Inspect the keyframe declarations: any layout-affecting name forces layout.
                if kf
                    .frames
                    .iter()
                    .flat_map(|(_, decls)| decls.iter())
                    .any(|d| ComputedStyle::decl_affects_layout(&d.name))
                {
                    return true;
                }
            }
        }
        false
    }

    /// Ids with running paint-only motion (transitions/animations that do not
    /// affect layout). Their boxes must be rebuilt each frame for fresh
    /// effective styles, but Taffy can be skipped.
    pub(crate) fn anim_paint_ids(&self) -> Vec<NodeId> {
        let mut out = Vec::new();
        for (id, st) in &self.anim {
            if st.transitions.is_empty() && st.animations.is_empty() {
                continue;
            }
            // Skip layout-affecting motion here; the full layout path covers it.
            // (Check per-node to avoid one width transition forcing all nodes paint-only.)
            let mut needs_layout = false;
            for t in &st.transitions {
                if ComputedStyle::is_layout_prop(t.prop) {
                    if t.prop != "all" {
                        needs_layout = true;
                        break;
                    }
                    if let Some(base) = self.styles.get(*id as usize).and_then(|s| s.as_ref()) {
                        if !t.from.layout_eq(base) {
                            needs_layout = true;
                            break;
                        }
                    } else {
                        needs_layout = true;
                        break;
                    }
                }
            }
            if !needs_layout {
                for a in &st.animations {
                    let running = self
                        .styles
                        .get(*id as usize)
                        .and_then(|s| s.as_ref())
                        .and_then(|b| b.animations.get(a.spec_index))
                        .is_some_and(|s| {
                            s.iterations.is_infinite()
                                || (self.now - a.start) as f32 - s.delay < s.duration * s.iterations
                        });
                    if !running {
                        continue;
                    }
                    if let Some(kf) = self.rules.keyframes.get(&a.name) {
                        if kf.frames.iter().flat_map(|(_, d)| d.iter()).any(|d| ComputedStyle::decl_affects_layout(&d.name)) {
                            needs_layout = true;
                            break;
                        }
                    }
                }
            }
            if !needs_layout {
                out.push(*id);
            }
        }
        out
    }

    /// Effective style of an element at the engine's current time.
    pub fn style_of(&self, id: NodeId) -> Option<Arc<ComputedStyle>> {
        let base = self.styles.get(id as usize)?.as_ref()?;
        if self.anim.contains_key(&id) {
            Some(self.effective_from(id, base, self.now))
        } else {
            Some(base.clone())
        }
    }

    pub fn pseudo_style_of(&self, id: NodeId, before: bool) -> Option<Arc<ComputedStyle>> {
        self.pseudo.get(&(id, before)).cloned()
    }

    pub(crate) fn doc_capacity(&self) -> usize {
        self.doc.capacity()
    }
}

fn extract_url(src: &str) -> Option<String> {
    let start = src.find("url(")? + 4;
    let rest = &src[start..];
    let end = rest.find(')')?;
    Some(rest[..end].trim().trim_matches(|c| c == '"' || c == '\'').to_string())
}

#[cfg(test)]
mod tests {
    use crate::{Engine, MapAssets};
    use rendrix_dom::Document;

    fn engine(html: &str, css: &str, w: f32, h: f32) -> Engine {
        let src = format!("<html><head><style>{css}</style></head><body>{html}</body></html>");
        let mut e = Engine::with_document(Document::parse(&src), Box::new(MapAssets::default()));
        e.set_viewport(w, h, 1.0);
        e.update(0.0);
        e
    }

    fn width_of(e: &Engine, id: &str) -> String {
        let n = e.doc.get_element_by_id(id).unwrap();
        e.computed_value(n, "width")
    }

    #[test]
    fn resize_reuses_ruleset_and_skips_restyle_without_viewport_deps() {
        // P2+P3: static sheets parse once; a resize then rebuilds the same RuleSet and,
        // with no @media and no viewport-relative values, skips the restyle entirely.
        let mut e = engine(r#"<div id="a"><span>x</span></div>"#, "body{margin:0} #a{width:100px} #a span{color:red}", 800.0, 600.0);
        assert!(!e.rules.has_media_rules && !e.rules.uses_viewport_units);
        let (len, full_before, w_before) = (e.rules.len(), e.stats.full_restyles, width_of(&e, "a"));
        assert!(len > 0);
        e.set_viewport(700.0, 600.0, 1.0);
        e.update(0.1);
        assert_eq!(e.stats.full_restyles, full_before, "P3 skips the resize restyle");
        assert_eq!(e.rules.len(), len, "P2 rebuilds the identical RuleSet without reparsing");
        assert_eq!(width_of(&e, "a"), w_before);
    }

    #[test]
    fn media_queries_and_viewport_units_still_restyle_on_resize() {
        let mut e = engine(r#"<div id="a"></div>"#, "#a{width:100px} @media (max-width: 500px){#a{width:50px}}", 800.0, 600.0);
        assert!(e.rules.has_media_rules);
        let full_before = e.stats.full_restyles;
        e.set_viewport(400.0, 600.0, 1.0);
        e.update(0.1);
        assert!(e.stats.full_restyles > full_before, "breakpoint flip must restyle");
        assert_eq!(width_of(&e, "a"), "50px");

        let mut v = engine(r#"<div id="a"></div>"#, "#a{width:50vw}", 800.0, 600.0);
        assert!(v.rules.uses_viewport_units);
        let full_before = v.stats.full_restyles;
        v.set_viewport(400.0, 600.0, 1.0);
        v.update(0.1);
        assert!(v.stats.full_restyles > full_before, "viewport units must restyle");
        assert_eq!(width_of(&v, "a"), "200px");
    }

    #[test]
    fn inline_viewport_style_forces_restyle_on_resize() {
        let mut e = engine(r#"<div id="a"></div>"#, "#a{width:100px}", 800.0, 600.0);
        assert!(!e.doc.inline_viewport_units);
        let a = e.doc.get_element_by_id("a").unwrap();
        e.doc.set_attr(a, "style", "width: 10vw");
        e.update(0.1);
        assert!(e.doc.inline_viewport_units);
        let full_before = e.stats.full_restyles;
        e.set_viewport(400.0, 600.0, 1.0);
        e.update(0.2);
        assert!(e.stats.full_restyles > full_before);
        assert_eq!(width_of(&e, "a"), "40px");
    }
}
