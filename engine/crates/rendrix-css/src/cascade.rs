//! The cascade: rule indexing, matching, ordering by origin/importance/specificity/source order,
//! custom property resolution, and computed style construction.

use crate::parser::{Declaration, FontFace, Keyframes, MediaContext, MediaQueryList, Rule, StyleRule, Stylesheet, SupportsCondition};
use crate::properties::{apply_declaration, StyleContext};
use crate::selector::{PseudoElement, Selector, SelectorElement};
use crate::style::{ComputedStyle, Display, Position};
use crate::values::{substitute_env, substitute_vars, value_uses_viewport_units};
use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Origin {
    UserAgent,
    Author,
}

#[derive(Debug)]
struct IndexedRule {
    selector: Selector,
    declarations: Arc<[Declaration]>,
    origin: Origin,
    order: u32,
    /// Cascade layer index assigned on first appearance; `None` = unlayered.
    layer: Option<u32>,
    /// Container query gating this rule (name + size condition).
    container: Option<(String, MediaQueryList)>,
}

#[derive(Debug, Default)]
pub struct RuleSet {
    rules: Vec<IndexedRule>,
    by_id: HashMap<String, Vec<u32>>,
    by_class: HashMap<String, Vec<u32>>,
    by_tag: HashMap<String, Vec<u32>>,
    universal: Vec<u32>,
    pub keyframes: HashMap<String, Keyframes>,
    pub font_faces: Vec<FontFace>,
    next_order: u32,
    /// Cascade layer names in first-appearance order.
    layer_names: HashMap<String, u32>,
    next_layer: u32,
    /// Some selector tests hover/active/focus on a compound other than its subject (e.g. `.a:hover .b`),
    /// so a state change can restyle descendants or siblings of the element that changed.
    pub state_deps_nonlocal: bool,
    /// Some selector depends on sibling position or siblings (`:nth-child`, `:first-child`, `+`, `~`, `:empty`).
    pub structural_deps: bool,
    /// The stylesheet contains at least one `@media` rule (whether or not it matched).
    pub has_media_rules: bool,
    /// The stylesheet contains at least one `@container` rule.
    pub has_container_rules: bool,
    /// Some declaration value can resolve against the viewport size (`vw`/`vh`/`vmin`/`vmax`,
    /// or a `var()` that might substitute one). Checked on resize to decide whether computed
    /// styles can be reused.
    pub uses_viewport_units: bool,
    /// Some declaration value contains a `%` (informational; percentages resolve at layout time
    /// and never force a restyle by themselves).
    pub uses_percent: bool,
}

/// Evaluate an `@supports` condition by asking the property parser whether each
/// declaration is actually accepted. Unknown properties/values and unsupported
/// functions evaluate to false, matching CSS feature detection.
fn supports_condition_matches(cond: &SupportsCondition) -> bool {
    match cond {
        SupportsCondition::Declaration(name, value) => {
            let parent = ComputedStyle::default();
            let mut style = ComputedStyle::default();
            apply_declaration(&mut style, &parent, name, value, &StyleContext::default())
        }
        SupportsCondition::Selector(s) => Selector::parse(s).is_some(),
        SupportsCondition::Not(c) => !supports_condition_matches(c),
        SupportsCondition::And(cs) => cs.iter().all(supports_condition_matches),
        SupportsCondition::Or(cs) => cs.iter().any(supports_condition_matches),
        SupportsCondition::Unsupported => false,
    }
}

impl RuleSet {
    pub fn new() -> Self {
        RuleSet::default()
    }

    /// Add every rule of `sheet` that applies under `media`.
    pub fn add_stylesheet(&mut self, sheet: &Stylesheet, origin: Origin, media: &MediaContext) {
        self.add_rules(&sheet.rules, origin, media, None, None);
    }

    fn layer_id(&mut self, name: &str) -> u32 {
        if let Some(id) = self.layer_names.get(name) {
            return *id;
        }
        let id = self.next_layer;
        self.next_layer += 1;
        self.layer_names.insert(name.to_string(), id);
        id
    }

    fn add_rules(
        &mut self,
        rules: &[Rule],
        origin: Origin,
        media: &MediaContext,
        layer: Option<u32>,
        container: Option<&(String, MediaQueryList)>,
    ) {
        for rule in rules {
            match rule {
                Rule::Style(r) => self.add_style_rule(r, origin, layer, container),
                Rule::Media(q, inner) => {
                    self.has_media_rules = true;
                    if q.matches(media) {
                        self.add_rules(inner, origin, media, layer, container);
                    }
                }
                Rule::Supports(cond, inner) => {
                    if supports_condition_matches(cond) {
                        self.add_rules(inner, origin, media, layer, container);
                    }
                }
                Rule::LayerStatement(names) => {
                    for name in names {
                        self.layer_id(name);
                    }
                }
                Rule::Container(name, query, inner) => {
                    self.has_container_rules = true;
                    self.add_rules(inner, origin, media, layer, Some(&(name.clone(), query.clone())));
                }
                Rule::Layer(name, inner) => {
                    let id = match name {
                        Some(n) => self.layer_id(n),
                        None => {
                            let id = self.next_layer;
                            self.next_layer += 1;
                            id
                        }
                    };
                    self.add_rules(inner, origin, media, Some(id), container);
                }
                Rule::Keyframes(k) => {
                    for (_, decls) in &k.frames {
                        self.note_viewport_flags(decls);
                    }
                    self.keyframes.insert(k.name.clone(), k.clone());
                }
                Rule::FontFace(f) => self.font_faces.push(f.clone()),
                Rule::Import(_, _) => {
                    // Resolved by `rendrix-core` (bundled-asset splice) before
                    // indexing; a bare parsed sheet ignores it defensively.
                }
            }
        }
    }

    /// Fold declaration dependency flags into the ruleset.
    fn note_viewport_flags(&mut self, decls: &[Declaration]) {
        for d in decls {
            if value_uses_viewport_units(&d.value) {
                self.uses_viewport_units = true;
            }
            if d.value.contains('%') {
                self.uses_percent = true;
            }
            if self.uses_viewport_units && self.uses_percent {
                break;
            }
        }
    }

    fn add_style_rule(&mut self, r: &StyleRule, origin: Origin, layer: Option<u32>, container: Option<&(String, MediaQueryList)>) {
        let decls: Arc<[Declaration]> = r.declarations.clone().into();
        self.note_viewport_flags(&r.declarations);
        for sel in &r.selectors {
            let state = |c: &crate::selector::Compound| {
                fn has(p: &crate::selector::PseudoClass) -> bool {
                    use crate::selector::PseudoClass::*;
                    match p {
                        Hover | Active | Focus | FocusVisible | FocusWithin | Checked => true,
                        Not(l) | Is(l) | Where(l) => l.iter().any(|s| s.compounds.iter().any(|c| c.pseudos.iter().any(has))),
                        Has(h) => h.iter().any(|x| x.selector.compounds.iter().any(|c| c.pseudos.iter().any(has))),
                        _ => false,
                    }
                }
                c.pseudos.iter().any(has)
            };
            let structural = {
                fn has(p: &crate::selector::PseudoClass) -> bool {
                    use crate::selector::PseudoClass::*;
                    match p {
                        FirstChild | LastChild | OnlyChild | NthChild(..) | NthLastChild(..) | NthOfType(..) | FirstOfType | LastOfType | Empty => true,
                        Not(l) | Is(l) | Where(l) => l.iter().any(|s| s.compounds.iter().any(|c| c.pseudos.iter().any(has))),
                        Has(_) => true,
                        _ => false,
                    }
                }
                sel.compounds.iter().any(|c| c.pseudos.iter().any(has))
                    || sel.combinators.iter().any(|c| matches!(c, crate::selector::Combinator::NextSibling | crate::selector::Combinator::SubsequentSibling))
            };
            if structural {
                self.structural_deps = true;
            }
            if sel.compounds.len() > 1 && sel.compounds[..sel.compounds.len() - 1].iter().any(state) {
                self.state_deps_nonlocal = true;
            }
            // `:has()` always observes other elements; state inside it is nonlocal
            // even when the `:has()` itself sits on the subject.
            {
                let has_inner_state = sel.compounds.iter().any(|c| {
                    c.pseudos.iter().any(|p| match p {
                        crate::selector::PseudoClass::Has(h) => h.iter().any(|x| x.selector.compounds.iter().any(state)),
                        _ => false,
                    })
                });
                if has_inner_state {
                    self.state_deps_nonlocal = true;
                }
            }
            let idx = self.rules.len() as u32;
            let subject = sel.compounds.last().unwrap();
            if let Some(id) = &subject.id {
                self.by_id.entry(id.clone()).or_default().push(idx);
            } else if let Some(c) = subject.classes.first() {
                self.by_class.entry(c.clone()).or_default().push(idx);
            } else if let Some(t) = &subject.tag {
                self.by_tag.entry(t.clone()).or_default().push(idx);
            } else {
                self.universal.push(idx);
            }
            self.rules.push(IndexedRule {
                selector: sel.clone(),
                declarations: decls.clone(),
                origin,
                order: self.next_order,
                layer,
                container: container.cloned(),
            });
            self.next_order += 1;
        }
    }

    pub fn len(&self) -> usize {
        self.rules.len()
    }

    pub fn is_empty(&self) -> bool {
        self.rules.is_empty()
    }

    /// Rules matching `el` (for `pseudo`, the originating element of that pseudo-element), sorted by
    /// ascending cascade precedence: (origin, specificity, source order).
    fn matching<E: SelectorElement>(&self, el: &E, pseudo: Option<PseudoElement>, sc: &StyleContext) -> Vec<&IndexedRule> {
        let mut candidates: Vec<u32> = Vec::new();
        if let Some(id) = el.id() {
            if let Some(v) = self.by_id.get(id) {
                candidates.extend_from_slice(v);
            }
        }
        // Class buckets: probe each class the element carries via a class attribute scan.
        // Elements that cache their split classes (the DOM does) skip the re-split.
        if let Some(cached) = el.classes() {
            for c in cached {
                if let Some(v) = self.by_class.get(c.as_str()) {
                    candidates.extend_from_slice(v);
                }
            }
        } else if let Some(class_attr) = el.attr("class") {
            for c in class_attr.split_whitespace() {
                if let Some(v) = self.by_class.get(c) {
                    candidates.extend_from_slice(v);
                }
            }
        }
        if let Some(v) = self.by_tag.get(el.tag()) {
            candidates.extend_from_slice(v);
        }
        candidates.extend_from_slice(&self.universal);
        candidates.sort_unstable();
        candidates.dedup();
        let mut hits: Vec<&IndexedRule> = candidates
            .into_iter()
            .map(|i| &self.rules[i as usize])
            .filter(|r| {
                let base = match pseudo {
                    None => r.selector.matches(el),
                    Some(p) => r.selector.pseudo_element == Some(p) && r.selector.matches_subject(el),
                };
                if !base {
                    return false;
                }
                match &r.container {
                    None => true,
                    Some((name, query)) => {
                        let mut cur = el.parent_element();
                        while let Some(a) = cur {
                            if let Some((cname, w, h)) = sc.container_sizes.get(&a.node_id()) {
                                // An unnamed query matches the nearest container of any name.
                                if name.is_empty() || cname == name {
                                    let mc = MediaContext {
                                        width: *w,
                                        height: *h,
                                        dpr: 1.0,
                                        dark: false,
                                        reduced_motion: false,
                                    };
                                    return query.matches(&mc);
                                }
                            }
                            cur = a.parent_element();
                        }
                        false
                    }
                }
            })
            .collect();
        // Unlayered rules outrank layered ones for normal declarations; `compute_style`
        // applies the reversed layer order for `!important` declarations.
        hits.sort_by_key(|r| (r.origin, r.layer.unwrap_or(u32::MAX), r.selector.specificity, r.order));
        hits
    }
}

/// Full cascade key: (tier, layer rank, id, class, type, source order, inline).
/// Layer order is reversed for `!important` declarations per CSS Cascade 5.
fn declaration_key(
    origin: Origin,
    important: bool,
    layer: Option<u32>,
    specificity: (u32, u32, u32),
    order: u32,
    inline: bool,
) -> (u32, u32, u32, u32, u32, u32, u32) {
    let tier = match (origin, important) {
        (Origin::UserAgent, false) => 0,
        (Origin::Author, false) => 1,
        (Origin::Author, true) => 2,
        (Origin::UserAgent, true) => 3,
    };
    let layer_rank = if important {
        layer.map(|i| u32::MAX - i).unwrap_or(0)
    } else {
        layer.unwrap_or(u32::MAX)
    };
    (
        tier,
        layer_rank,
        specificity.0,
        specificity.1,
        specificity.2,
        order,
        inline as u32,
    )
}

/// Compute the style of one element.
///
/// * `inline` are declarations from the `style` attribute (highest author normal precedence).
/// * `parent` is the parent's computed style (`None` for the root).
pub fn compute_style<E: SelectorElement>(
    el: &E,
    rules: &RuleSet,
    inline: &[Declaration],
    parent: Option<&ComputedStyle>,
    sc: &StyleContext,
    pseudo: Option<PseudoElement>,
) -> ComputedStyle {
    let default_parent = ComputedStyle::default();
    let parent_style = parent.unwrap_or(&default_parent);

    // Gather declarations with their full cascade key. Last-write-wins after sorting
    // gives origin, layer order, specificity, importance and source order in one pass.
    let mut entries: Vec<((u32, u32, u32, u32, u32, u32, u32), &Declaration)> = Vec::new();
    for r in rules.matching(el, pseudo, sc) {
        for d in r.declarations.iter() {
            entries.push((declaration_key(r.origin, d.important, r.layer, r.selector.specificity, r.order, false), d));
        }
    }
    if pseudo.is_none() {
        for d in inline {
            entries.push((declaration_key(Origin::Author, d.important, None, (u32::MAX, u32::MAX, u32::MAX), u32::MAX, true), d));
        }
    }
    entries.sort_by_key(|(k, _)| *k);
    let ordered: Vec<&Declaration> = entries.into_iter().map(|(_, d)| d).collect();

    // Custom properties (inherited): start from the parent's, then apply in cascade order.
    // P5: the common case defines no `--x` at all, so scan first and only clone the map
    // when something will actually be inserted. `inherit_from` already shared the parent's Arc.
    let mut any_custom = false;
    for d in &ordered {
        if d.name.starts_with("--") {
            any_custom = true;
            break;
        }
    }
    let mut owned_custom: Option<BTreeMap<String, String>> = None;
    let custom: &BTreeMap<String, String> = if any_custom {
        let mut m = (*parent_style.custom).clone();
        for d in &ordered {
            if d.name.starts_with("--") {
                m.insert(d.name.clone(), d.value.clone());
            }
        }
        owned_custom = Some(m);
        owned_custom.as_ref().unwrap()
    } else {
        &parent_style.custom
    };

    let mut style = ComputedStyle::inherit_from(parent_style);
    // Un-inherit nothing else: inherit_from already gave initial values for non-inherited props.

    let value_of = |d: &Declaration| -> Option<String> {
        let v = if d.value.contains("env(") { substitute_env(&d.value, sc.safe_area, sc.keyboard_h)? } else { d.value.clone() };
        if v.contains("var(") {
            substitute_vars(&v, &custom)
        } else {
            Some(v)
        }
    };

    // Pass 1: properties others depend on (font-size → em, color → currentcolor).
    for d in &ordered {
        if d.name.starts_with("--") {
            continue;
        }
        if matches!(d.name.as_str(), "font-size" | "font" | "color") {
            if let Some(v) = value_of(d) {
                apply_declaration(&mut style, parent_style, &d.name, &v, sc);
            }
        }
    }
    // Pass 2: everything else, in cascade order.
    for d in &ordered {
        if d.name.starts_with("--") || matches!(d.name.as_str(), "font-size" | "font" | "color") {
            continue;
        }
        if let Some(v) = value_of(d) {
            apply_declaration(&mut style, parent_style, &d.name, &v, sc);
        }
    }

    // Used-value adjustments that CSS applies at computed-value time.
    if matches!(style.position, Position::Absolute | Position::Fixed) || !matches!(style.float, crate::style::Float::None) {
        style.display = match style.display {
            Display::Inline | Display::InlineBlock => Display::Block,
            Display::InlineFlex => Display::Flex,
            Display::InlineGrid => Display::Grid,
            Display::InlineTable => Display::Table,
            // Floated internal table boxes and columns lose their table role.
            Display::TableCell | Display::TableCaption | Display::ListItem => Display::Block,
            d => d,
        };
    }
    if let Some(m) = owned_custom {
        style.custom = Arc::new(m);
    }
    style
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::color::Color;
    use crate::selector::ElementState;
    use crate::values::Dim;

    /// A tiny element tree for testing the cascade in isolation.
    #[derive(Clone)]
    struct El {
        tree: std::rc::Rc<Vec<N>>,
        idx: usize,
    }
    struct N {
        tag: &'static str,
        id: Option<&'static str>,
        class: Option<&'static str>,
        parent: Option<usize>,
        hover: bool,
    }
    impl SelectorElement for El {
        fn tag(&self) -> &str {
            self.tree[self.idx].tag
        }
        fn id(&self) -> Option<&str> {
            self.tree[self.idx].id
        }
        fn has_class(&self, c: &str) -> bool {
            self.tree[self.idx].class.is_some_and(|cl| cl.split_whitespace().any(|x| x == c))
        }
        fn attr(&self, name: &str) -> Option<&str> {
            match name {
                "class" => self.tree[self.idx].class,
                "id" => self.tree[self.idx].id,
                _ => None,
            }
        }
        fn parent_element(&self) -> Option<Self> {
            self.tree[self.idx].parent.map(|p| El { tree: self.tree.clone(), idx: p })
        }
        fn prev_sibling_element(&self) -> Option<Self> {
            None
        }
        fn next_sibling_element(&self) -> Option<Self> {
            None
        }
        fn has_state(&self, s: ElementState) -> bool {
            s == ElementState::Hover && self.tree[self.idx].hover
        }
        fn is_root(&self) -> bool {
            self.tree[self.idx].parent.is_none()
        }
        fn is_empty(&self) -> bool {
            true
        }
    }

    fn tree(hover: bool) -> (El, El) {
        let t = std::rc::Rc::new(vec![
            N { tag: "div", id: Some("app"), class: None, parent: None, hover: false },
            N { tag: "span", id: None, class: Some("a b"), parent: Some(0), hover },
        ]);
        (El { tree: t.clone(), idx: 0 }, El { tree: t, idx: 1 })
    }

    fn rules(css: &str) -> RuleSet {
        let mut rs = RuleSet::new();
        rs.add_stylesheet(&Stylesheet::parse(css), Origin::Author, &MediaContext::default());
        rs
    }

    fn compute(el: &El, rs: &RuleSet, parent: Option<&ComputedStyle>) -> ComputedStyle {
        compute_style(el, rs, &[], parent, &StyleContext::default(), None)
    }

    #[test]
    fn ruleset_tracks_media_and_viewport_dependencies() {
        // P2/P3: presence of @media / viewport-relative declarations is recorded even when
        // the media does not currently match, so resize can decide whether restyle is needed.
        let plain = rules("div { color: red; width: 50% }");
        assert!(!plain.has_media_rules && !plain.uses_viewport_units);
        assert!(plain.uses_percent, "percent is tracked (layout-resolved, informational)");

        let with_media = rules("@media (max-width: 100px) { div { color: green } }");
        assert!(with_media.has_media_rules);

        let vw = rules("div { width: 50vw; height: 10vh }");
        assert!(vw.uses_viewport_units);

        let var_indirect = rules("div { width: var(--w) }");
        assert!(var_indirect.uses_viewport_units, "var() may substitute viewport units");

        let kf = rules("@keyframes k { from { width: 0vw } to { width: 10vw } }");
        assert!(kf.uses_viewport_units, "keyframe declarations count too");

        let ua = RuleSet::new();
        assert!(!ua.has_media_rules && !ua.uses_viewport_units);
    }

    #[test]
    fn supports_conditions_are_really_evaluated() {
        let css = r#"
          @supports (display: grid) { div { color: lime } }
          @supports (bogus-property: whatever) { div { color: red } }
          @supports not (bogus-property: whatever) { div { background: blue } }
          @supports (display: grid) and (color: red) { span { color: yellow } }
          @supports (display: grid) or (bogus: x) { span { background: pink } }
        "#;
        let rs = rules(css);
        let (div, span) = tree(false);
        let d = compute(&div, &rs, None);
        let s = compute(&span, &rs, None);
        assert_eq!(d.color, Color::from_u8(0, 255, 0, 1.0), "supported declaration applies");
        assert_eq!(d.background_color, Color::from_u8(0, 0, 255, 1.0), "not() applies when inner is unsupported");
        assert_eq!(s.color, Color::from_u8(255, 255, 0, 1.0), "and() applies when both supported");
        assert_eq!(s.background_color, Color::from_u8(255, 192, 203, 1.0), "or() applies when one supported");
    }

    #[test]
    fn layers_order_normally_and_reverse_for_important() {
        let css = r#"
          @layer first { div { color: red !important; background: red } }
          @layer second { #app { color: green !important } div { background: blue } }
          div { color: purple; background: yellow }
        "#;
        let rs = rules(css);
        let (div, _) = tree(false);
        let c = compute(&div, &rs, None);
        assert_eq!(c.background_color, Color::from_u8(255, 255, 0, 1.0), "unlayered normal beats all layers");
        assert_eq!(c.color, Color::from_u8(255, 0, 0, 1.0), "earlier layer wins for !important despite specificity");
    }

    #[test]
    fn specificity_beats_order() {
        let (_, span) = tree(false);
        let rs = rules("#app span { color: red } span.a { color: blue } span { color: green }");
        let s = compute(&span, &rs, None);
        assert_eq!(s.color, Color::from_u8(255, 0, 0, 1.0));
    }

    #[test]
    fn later_wins_on_tie_and_important_wins() {
        let (_, span) = tree(false);
        let rs = rules(".a { color: red } .b { color: blue } .a { margin: 1px !important } span { margin: 9px }");
        let s = compute(&span, &rs, None);
        assert_eq!(s.color, Color::from_u8(0, 0, 255, 1.0));
        assert_eq!(s.margin[0], Dim::Px(1.0));
    }

    #[test]
    fn inline_beats_rules_but_not_important() {
        let (_, span) = tree(false);
        let rs = rules("#app span { color: red; width: 10px !important }");
        let inline = crate::parser::parse_declarations("color: blue; width: 99px");
        let s = compute_style(&span, &rs, &inline, None, &StyleContext::default(), None);
        assert_eq!(s.color, Color::from_u8(0, 0, 255, 1.0));
        assert_eq!(s.width, Dim::Px(10.0));
    }

    #[test]
    fn inheritance_and_non_inherited() {
        let (div, span) = tree(false);
        let rs = rules("#app { color: red; padding: 10px }");
        let ds = compute(&div, &rs, None);
        let ss = compute(&span, &rs, Some(&ds));
        assert_eq!(ss.color, ds.color, "color inherits");
        assert_eq!(ss.padding[0], Dim::Px(0.0), "padding does not");
    }

    #[test]
    fn custom_properties_inherit_and_resolve() {
        let (div, span) = tree(false);
        let rs = rules("#app { --gap: 12px; --pad: var(--gap) } span { margin: var(--gap); padding: var(--pad, 1px); width: var(--none, 5px) }");
        let ds = compute(&div, &rs, None);
        let ss = compute(&span, &rs, Some(&ds));
        assert_eq!(ss.margin[0], Dim::Px(12.0));
        assert_eq!(ss.padding[0], Dim::Px(12.0));
        assert_eq!(ss.width, Dim::Px(5.0));
    }

    #[test]
    fn state_pseudo_classes_and_media() {
        let (_, span) = tree(true);
        let mut rs = RuleSet::new();
        let css = "span { color: red } span:hover { color: blue } @media (max-width: 100px) { span { color: green } }";
        rs.add_stylesheet(&Stylesheet::parse(css), Origin::Author, &MediaContext::default());
        assert_eq!(compute(&span, &rs, None).color, Color::from_u8(0, 0, 255, 1.0));
        let (_, span) = tree(false);
        assert_eq!(compute(&span, &rs, None).color, Color::from_u8(255, 0, 0, 1.0));
        let mut small = RuleSet::new();
        small.add_stylesheet(&Stylesheet::parse(css), Origin::Author, &MediaContext { width: 50.0, ..Default::default() });
        assert_eq!(compute(&span, &small, None).color, Color::from_u8(0, 128, 0, 1.0));
    }

    #[test]
    fn user_agent_loses_to_author() {
        let (_, span) = tree(false);
        let mut rs = RuleSet::new();
        rs.add_stylesheet(&Stylesheet::parse("span { display: block; color: red }"), Origin::UserAgent, &MediaContext::default());
        rs.add_stylesheet(&Stylesheet::parse("span { color: blue }"), Origin::Author, &MediaContext::default());
        let s = compute(&span, &rs, None);
        assert_eq!(s.display, Display::Block);
        assert_eq!(s.color, Color::from_u8(0, 0, 255, 1.0));
    }

    #[test]
    fn absolute_blockifies() {
        let (_, span) = tree(false);
        let rs = rules("span { position: absolute }");
        assert_eq!(compute(&span, &rs, None).display, Display::Block);
    }

    #[test]
    fn float_blockifies_inline_level_boxes() {
        let (_, span) = tree(false);
        let rs = rules("span { float: left }");
        assert_eq!(compute(&span, &rs, None).display, Display::Block, "floated inlines become blocks");
        let rs = rules("span { float: none }");
        assert_eq!(compute(&span, &rs, None).display, Display::Inline);
    }
}
