//! Parsing and cascade tests for the paint-gap subset: `filter` / `backdrop-filter`,
//! `clip-path`, `mask-image` / `mask`, `text-shadow`, plus their stacking-context effects.

use rendrix_css::{apply_declaration, ClipPath, ComputedStyle, FilterOp, StyleContext};

fn sc() -> StyleContext {
    StyleContext::default()
}

fn styled(decls: &[(&str, &str)]) -> ComputedStyle {
    let parent = ComputedStyle::default();
    let mut s = ComputedStyle::inherit_from(&parent);
    for (k, v) in decls {
        assert!(apply_declaration(&mut s, &parent, k, v, &sc()), "{k}: {v} should parse");
    }
    s
}

fn try_apply(name: &str, value: &str) -> Option<ComputedStyle> {
    let parent = ComputedStyle::default();
    let mut s = ComputedStyle::inherit_from(&parent);
    apply_declaration(&mut s, &parent, name, value, &sc()).then(|| s)
}

#[test]
fn filter_subset_parses_and_rejects() {
    let s = styled(&[("filter", "blur(4px) brightness(50%) grayscale(1)")]);
    assert_eq!(s.filter, vec![FilterOp::Blur(4.0), FilterOp::Brightness(0.5), FilterOp::Grayscale(1.0)]);
    assert!(styled(&[("filter", "none")]).filter.is_empty());
    assert!(styled(&[("backdrop-filter", "blur(2px)")]).backdrop_filter == vec![FilterOp::Blur(2.0)]);
    // Unknown functions and negative radii are invalid (declaration ignored).
    assert!(try_apply("filter", "hue-rotate(10deg)").is_none());
    assert!(try_apply("filter", "blur(-1px)").is_none());
    assert!(try_apply("filter", "brightness(-0.5)").is_none());
    assert!(try_apply("filter", "").is_none());
    // No-op detection keeps the fast path: identity filters emit no display-list markers.
    assert!(FilterOp::Brightness(1.0).is_noop());
    assert!(FilterOp::Grayscale(0.0).is_noop());
    assert!(FilterOp::Blur(0.0).is_noop());
    assert!(!FilterOp::Blur(0.5).is_noop());
}

#[test]
fn clip_path_basic_shapes_parse() {
    let s = styled(&[("clip-path", "inset(10px 20px)")]);
    match s.clip_path {
        ClipPath::Inset { top, right, bottom, left } => {
            assert_eq!((top.px(), right.px(), bottom.px(), left.px()), (Some(10.0), Some(20.0), Some(10.0), Some(20.0)));
        }
        other => panic!("inset: {other:?}"),
    }
    // `round` radii are accepted and ignored.
    assert!(matches!(try_apply("clip-path", "inset(5px round 50%)").unwrap().clip_path, ClipPath::Inset { .. }));
    let s = styled(&[("clip-path", "circle(50px at 25% 75%)")]);
    match s.clip_path {
        ClipPath::Circle { r, cx, cy } => assert_eq!((r.px(), cx, cy), (Some(50.0), rendrix_css::Dim::Percent(25.0), rendrix_css::Dim::Percent(75.0))),
        other => panic!("circle: {other:?}"),
    }
    // Centre defaults to 50%/50%.
    assert!(matches!(
        try_apply("clip-path", "circle(10px)").unwrap().clip_path,
        ClipPath::Circle { cx: rendrix_css::Dim::Percent(50.0), .. }
    ));
    let s = styled(&[("clip-path", "ellipse(30px 20px at 10px 10px)")]);
    assert!(matches!(s.clip_path, ClipPath::Ellipse { .. }));
    let s = styled(&[("clip-path", "polygon(0px 0px, 10px 0px, 5px 5px)")]);
    match s.clip_path {
        ClipPath::Polygon(pts) => assert_eq!(pts.len(), 3),
        other => panic!("polygon: {other:?}"),
    }
    assert!(matches!(try_apply("clip-path", "none").unwrap().clip_path, ClipPath::None));
    // Polygons need ≥ 3 points; unknown shapes are invalid.
    assert!(try_apply("clip-path", "polygon(0px 0px, 1px 1px)").is_none());
    assert!(try_apply("clip-path", "url(#c)").is_none());
    assert!(try_apply("clip-path", "circle(closest-side)").is_none());
}

#[test]
fn text_shadow_and_mask_parse() {
    let s = styled(&[("text-shadow", "1px 2px 3px red")]);
    assert_eq!(s.text_shadow.len(), 1);
    let sh = &s.text_shadow[0];
    assert_eq!((sh.dx, sh.dy, sh.blur), (1.0, 2.0, 3.0));
    assert!(!sh.inset);
    assert_eq!(s.text_shadow[0].color, rendrix_css::Color::from_u8(255, 0, 0, 1.0));
    // Blur and color are optional; `inset` and spread are rejected.
    assert_eq!(try_apply("text-shadow", "2px 2px blue").unwrap().text_shadow[0].blur, 0.0);
    assert!(try_apply("text-shadow", "1px 1px 1px 1px red").is_none());
    assert!(try_apply("text-shadow", "inset 1px 1px red").is_none());
    assert!(matches!(try_apply("text-shadow", "none").unwrap().text_shadow[..], []));
    // Mask: url extraction for both longhand and shorthand.
    assert_eq!(try_apply("mask-image", "url(mask.png)").unwrap().mask_image.as_deref(), Some("mask.png"));
    assert_eq!(try_apply("mask-image", "url(\"a/b.png\")").unwrap().mask_image.as_deref(), Some("a/b.png"));
    assert!(try_apply("mask-image", "none").unwrap().mask_image.is_none());
    assert_eq!(try_apply("mask", "url(m.png) alpha").unwrap().mask_image.as_deref(), Some("m.png"));
    assert!(try_apply("mask", "none").unwrap().mask_image.is_none());
}

#[test]
fn paint_props_stacking_inheritance_and_copy() {
    // Filter, backdrop-filter, clip-path and mask all establish a stacking context.
    for (k, v) in [("filter", "blur(1px)"), ("backdrop-filter", "grayscale(1)"), ("clip-path", "inset(0)"), ("mask-image", "url(m.png)")] {
        let s = styled(&[(k, v)]);
        assert!(s.creates_stacking_context(), "{k} creates a stacking context");
    }
    assert!(!ComputedStyle::default().creates_stacking_context());
    // Identity-only filter lists still parse (paint skips them, but the style is kept).
    assert!(styled(&[("filter", "brightness(1)")]).creates_stacking_context());
    // `text-shadow` inherits like other text properties; the box effects do not.
    let parent = styled(&[("text-shadow", "1px 1px red"), ("filter", "blur(1px)"), ("clip-path", "inset(1px)"), ("mask-image", "url(m.png)")]);
    let child = ComputedStyle::inherit_from(&parent);
    assert_eq!(child.text_shadow, parent.text_shadow);
    assert!(child.filter.is_empty() && child.backdrop_filter.is_empty());
    assert!(matches!(child.clip_path, ClipPath::None));
    assert!(child.mask_image.is_none());
}
