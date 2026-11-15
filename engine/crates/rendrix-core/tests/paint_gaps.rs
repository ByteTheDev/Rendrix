//! Engine integration tests for the SVG + paint gaps subset: inline/file SVG raster,
//! `filter`, `clip-path`, `text-shadow`, `mask-image`, `backdrop-filter`, group opacity,
//! and `position: sticky`. GPU pixel tests skip gracefully when no adapter is present
//! (same pattern as `rendrix-gfx`'s own renderer tests).

use rendrix_core::{Engine, MapAssets};
use rendrix_dom::Document;
use rendrix_gfx::{DisplayList, GpuContext, Item, Renderer, TextSystem};

fn engine(html: &str, css: &str, w: f32, h: f32) -> Engine {
    let src = format!("<html><head><style>{css}</style></head><body>{html}</body></html>");
    let mut e = Engine::with_document(Document::parse(&src), Box::new(MapAssets::default()));
    e.set_viewport(w, h, 1.0);
    e.update(0.0);
    e
}

fn engine_with_assets(html: &str, css: &str, assets: MapAssets) -> Engine {
    let src = format!("<html><head><style>{css}</style></head><body>{html}</body></html>");
    let mut e = Engine::with_document(Document::parse(&src), Box::new(assets));
    e.set_viewport(400.0, 300.0, 1.0);
    e.update(0.0);
    e
}

fn by_id(e: &Engine, id: &str) -> u32 {
    e.doc.get_element_by_id(id).unwrap_or_else(|| panic!("no #{id}"))
}

fn near(a: f32, b: f32) -> bool {
    (a - b).abs() < 1.0
}

/// Render an engine frame headlessly; `None` when no GPU adapter is present.
fn render_frame(e: &Engine) -> Option<Vec<u8>> {
    let ctx = GpuContext::headless().ok()?;
    let mut r = Renderer::new(ctx.device, ctx.queue, wgpu::TextureFormat::Rgba8Unorm);
    // Upload decoded images (SVG rasters land here through the same path as PNGs).
    for up in e.take_image_uploads() {
        r.upload_image_as(up.id, &up.rgba, up.width, up.height);
    }
    let list = e.display_list().clone();
    let (w, h) = (list.width as u32, list.height as u32);
    let mut t = TextSystem::new();
    Some(r.render_to_rgba(&list, &mut t, (w, h), 1.0))
}

fn px(img: &[u8], w: u32, x: u32, y: u32) -> [u8; 4] {
    let i = ((y * w + x) * 4) as usize;
    [img[i], img[i + 1], img[i + 2], img[i + 3]]
}

fn has_marker(list: &DisplayList, name: &str) -> bool {
    list.items.iter().any(|i| match (i, name) {
        (Item::PushFilter(_), "filter") => true,
        (Item::PopFilter, "filter") => true,
        (Item::PushBackdrop(_), "backdrop") => true,
        (Item::PopBackdrop, "backdrop") => true,
        (Item::PushEllipseClip { .. }, "ellipse") => true,
        (Item::PushOpacity(_), "opacity") => true,
        _ => false,
    })
}

const RED_SVG: &[u8] = br##"<svg width="40" height="20"><rect x="0" y="0" width="40" height="20" fill="#ff0000"/><circle cx="30" cy="10" r="6" fill="rgb(0,0,255)"/></svg>"##;

fn svg_available() -> bool {
    rendrix_gfx::svg::rasterize_at_intrinsic(RED_SVG).is_some()
}

// ---- SVG ----------------------------------------------------------------------------

#[test]
fn svg_file_img_has_intrinsic_size_and_paints_image() {
    if !svg_available() {
        eprintln!("svg rasterizer unavailable, skipping");
        return;
    }
    let mut assets = MapAssets::default();
    assets.0.insert("r.svg".into(), RED_SVG.to_vec());
    let e = engine_with_assets(r#"<img id="i" src="r.svg">"#, "body{margin:0}", assets);
    let r = e.element_rect(by_id(&e, "i")).expect("img has a box");
    assert!(near(r[2], 40.0) && near(r[3], 20.0), "intrinsic SVG size: {r:?}");
    assert!(e.display_list().items.iter().any(|i| matches!(i, Item::Image(_))), "svg paints as an image quad");
    // Pixel: the rasterized upload is red on the left, blue at the circle.
    let Some(img) = render_frame(&e) else { eprintln!("no GPU adapter, skipping"); return };
    assert_eq!(px(&img, 400, 4, 4), [255, 0, 0, 255]);
    let c = px(&img, 400, 30, 10);
    assert!(c[2] > 200 && c[0] < 60, "circle paints blue: {c:?}");
}

#[test]
fn svg_viewbox_only_sizes_from_viewbox() {
    if !svg_available() {
        eprintln!("svg rasterizer unavailable, skipping");
        return;
    }
    let vb = br##"<svg viewBox="0 0 100 50"><rect x="0" y="0" width="100" height="50" fill="green"/></svg>"##;
    let (w, h) = rendrix_gfx::svg::intrinsic_size(vb).expect("viewBox intrinsic size");
    assert_eq!((w, h), (100, 50));
}

#[test]
fn inline_svg_paints_textured_quad() {
    if !svg_available() {
        eprintln!("svg rasterizer unavailable, skipping");
        return;
    }
    let e = engine(
        r##"<svg id="s" width="40" height="20"><rect x="0" y="0" width="40" height="20" fill="#00ff00"/></svg>"##,
        "body{margin:0} #s{display:block;width:40px;height:20px}",
        400.0,
        300.0,
    );
    let images: Vec<_> = e.display_list().items.iter().filter_map(|i| if let Item::Image(im) = i { Some(im) } else { None }).collect();
    assert_eq!(images.len(), 1, "inline svg composites as one textured quad");
    assert!(near(images[0].rect[2], 40.0) && near(images[0].rect[3], 20.0), "{:?}", images[0].rect);
    let Some(img) = render_frame(&e) else { eprintln!("no GPU adapter, skipping"); return };
    assert_eq!(px(&img, 400, 4, 4), [0, 255, 0, 255]);
}

// ---- filter -------------------------------------------------------------------------

#[test]
fn filter_metadata_and_display_list_markers() {
    let e = engine(r#"<div id="a"></div>"#, "body{margin:0} #a{width:50px;height:50px;filter:blur(3px) brightness(0.5)}", 400.0, 300.0);
    let a = by_id(&e, "a");
    assert_eq!(e.computed_value(a, "filter"), "blur(3px) brightness(0.5)");
    assert!(has_marker(e.display_list(), "filter"));
    // Identity-only filters parse but emit no markers (fast path).
    let e2 = engine(r#"<div id="b"></div>"#, "body{margin:0} #b{width:50px;height:50px;filter:brightness(1)}", 400.0, 300.0);
    assert_eq!(e2.computed_value(by_id(&e2, "b"), "filter"), "brightness(1)");
    assert!(!has_marker(e2.display_list(), "filter"));
}

#[test]
fn filter_brightness_darkens_and_grayscale_desaturates() {
    let e = engine(
        r#"<div id="a"></div><div id="b"></div>"#,
        "body{margin:0} div{width:40px;height:40px} #a{background:#ff0000;filter:brightness(0.5)} #b{position:absolute;top:50px;background:#ff0000;filter:grayscale(1)}",
        200.0,
        200.0,
    );
    let Some(img) = render_frame(&e) else { eprintln!("no GPU adapter, skipping"); return };
    let dark = px(&img, 200, 20, 20);
    assert!(dark[0] > 110 && dark[0] < 145 && dark[1] < 10 && dark[2] < 10, "half brightness red: {dark:?}");
    let gray = px(&img, 200, 20, 70);
    // luma(1,0,0) ≈ 0.2126 → ~54.
    assert!((gray[0] as i32 - 54).abs() < 6 && (gray[1] as i32 - gray[0] as i32).abs() < 3 && (gray[2] as i32 - gray[0] as i32).abs() < 3, "grayscale red: {gray:?}");
}

#[test]
fn filter_blur_softens_edges() {
    let e = engine(
        r#"<div id="a"></div>"#,
        "body{margin:0} #a{position:absolute;left:10px;top:10px;width:40px;height:40px;background:#ff0000;filter:blur(6px)}",
        200.0,
        200.0,
    );
    // Paint emits one rect; the renderer expands it into centre + 4 offset copies
    // (batch-preserving box-blur approximation — instance counts covered in gfx tests).
    assert_eq!(e.display_list().items.iter().filter(|i| matches!(i, Item::Rect(_))).count(), 1);
    let Some(img) = render_frame(&e) else { eprintln!("no GPU adapter, skipping"); return };
    assert_eq!(px(&img, 200, 30, 30), [255, 0, 0, 255], "interior stays solid");
    // blur(6px) → ±3px halo copies at 25%: 2px outside the box is pinkish, 6px out is white.
    let halo = px(&img, 200, 8, 30);
    assert!(halo[0] > 240 && halo[1] > 100 && halo[1] < 230 && halo[2] == halo[1], "halo bleeds onto white: {halo:?}");
    assert_eq!(px(&img, 200, 4, 30), [255, 255, 255, 255], "past the halo stays background");
}

// ---- clip-path ----------------------------------------------------------------------

#[test]
fn clip_path_inset_and_circle_clip() {
    let e = engine(
        r#"<div id="a"></div><div id="b"></div>"#,
        "body{margin:0} div{width:40px;height:40px} #a{background:#00ff00;clip-path:inset(0 20px 0 0)} #b{position:absolute;top:50px;background:#ff0000;clip-path:circle(10px at 20px 20px)}",
        200.0,
        200.0,
    );
    let a = by_id(&e, "a");
    assert_eq!(e.computed_value(a, "clip-path"), "inset(0px, 20px, 0px, 0px)");
    let Some(img) = render_frame(&e) else { eprintln!("no GPU adapter, skipping"); return };
    assert_eq!(px(&img, 200, 10, 20), [0, 255, 0, 255], "inside the inset");
    assert_eq!(px(&img, 200, 30, 20), [255, 255, 255, 255], "right 20px clipped away");
    assert_eq!(px(&img, 200, 20, 70), [255, 0, 0, 255], "circle centre paints");
    assert_eq!(px(&img, 200, 2, 52), [255, 255, 255, 255], "circle corner clipped");
}

// ---- text-shadow --------------------------------------------------------------------

#[test]
fn text_shadow_emits_offset_copy() {
    let e = engine(
        r#"<div id="a">Hi</div>"#,
        "body{margin:0} #a{font-size:20px;color:#000000;text-shadow:2px 0px #ff0000}",
        200.0,
        100.0,
    );
    let a = by_id(&e, "a");
    assert_eq!(e.computed_value(a, "text-shadow"), "2px 0px 0px rgb(255, 0, 0)");
    let texts: Vec<_> = e.display_list().items.iter().filter_map(|i| if let Item::Text(t) = i { Some(t) } else { None }).collect();
    assert_eq!(texts.len(), 2, "shadow copy beneath the main text, got {}", texts.len());
    // Emission order is back-to-front: the shadow copy first, then the main text.
    assert!(near(texts[1].x + 2.0, texts[0].x) && near(texts[0].y, texts[1].y), "shadow offset by (2, 0)");
    let shadow_runs = &texts[0].content.runs;
    assert!(shadow_runs.iter().all(|r| r.style.color == rendrix_css::Color::from_u8(255, 0, 0, 1.0)));
}

// ---- mask / backdrop ----------------------------------------------------------------

fn gray_png(gray: u8, alpha: u8) -> Vec<u8> {
    let img = image::RgbaImage::from_pixel(4, 4, image::Rgba([gray, gray, gray, alpha]));
    let mut png = Vec::new();
    image::DynamicImage::ImageRgba8(img).write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png).unwrap();
    png
}

#[test]
fn mask_average_alpha_modulates_group() {
    let mut assets = MapAssets::default();
    assets.0.insert("half.png".into(), gray_png(255, 128));
    assets.0.insert("full.png".into(), gray_png(255, 255));
    let e = engine_with_assets(
        r#"<div id="a"></div><div id="b"></div>"#,
        "body{margin:0} div{width:40px;height:40px;background:#ff0000} #a{mask-image:url(half.png)} #b{position:absolute;top:50px;mask-image:url(full.png)}",
        assets,
    );
    // A uniform 50%-alpha mask folds one PushOpacity(0.5) over the subtree.
    let ops: Vec<f32> = e.display_list().items.iter().filter_map(|i| if let Item::PushOpacity(o) = i { Some(*o) } else { None }).collect();
    assert!(ops.iter().any(|o| (*o - 0.5).abs() < 0.02), "half mask → opacity 0.5, got {ops:?}");
    // A fully-opaque mask is a no-op (fast path: no marker).
    assert!(!ops.iter().any(|o| (*o - 1.0).abs() < 1e-6), "opaque mask adds no marker: {ops:?}");
    let Some(img) = render_frame(&e) else { eprintln!("no GPU adapter, skipping"); return };
    let masked = px(&img, 400, 20, 20);
    assert!(masked[0] > 240 && (masked[1] as i32 - 127).abs() < 12, "50% red over white: {masked:?}");
    assert_eq!(px(&img, 400, 20, 70), [255, 0, 0, 255], "opaque mask keeps full red");
}

#[test]
fn backdrop_filter_is_stacking_marker_without_visual_change() {
    let css = "body{margin:0} #bg{width:100px;height:100px;background:#ff0000} #fg{position:absolute;top:0;left:0;width:100px;height:100px;background:rgba(0,0,255,0.5);backdrop-filter:blur(8px)}";
    let e = engine(r#"<div id="bg"></div><div id="fg"></div>"#, css, 200.0, 200.0);
    assert!(has_marker(e.display_list(), "backdrop"));
    let fg = by_id(&e, "fg");
    assert_eq!(e.computed_value(fg, "backdrop-filter"), "blur(8px)");
    // Documented approximation: no visual change vs no backdrop-filter (needs offscreen pass).
    let css2 = css.replace(";backdrop-filter:blur(8px)", "");
    let plain = engine(r#"<div id="bg"></div><div id="fg"></div>"#, &css2, 200.0, 200.0);
    let (Some(a), Some(b)) = (render_frame(&e), render_frame(&plain)) else { eprintln!("no GPU adapter, skipping"); return };
    assert_eq!(a, b, "backdrop-filter is currently a parsing + stacking marker only");
}

// ---- group opacity ------------------------------------------------------------------

#[test]
fn nested_opacity_multiplies_as_group() {
    let e = engine(
        r#"<div id="p"><div id="c"></div></div>"#,
        "body{margin:0} #p{width:40px;height:40px;opacity:0.5} #c{width:40px;height:40px;background:#ff0000;opacity:0.5}",
        200.0,
        200.0,
    );
    assert!(has_marker(e.display_list(), "opacity"));
    let Some(img) = render_frame(&e) else { eprintln!("no GPU adapter, skipping"); return };
    let c = px(&img, 200, 20, 20);
    // 0.5 * 0.5 = 0.25 red over white → (255, 191, 191).
    assert_eq!(c[0], 255);
    assert!((c[1] as i32 - 191).abs() < 4 && (c[2] as i32 - 191).abs() < 4, "group opacity 0.25: {c:?}");
}

// ---- sticky -------------------------------------------------------------------------

#[test]
fn sticky_clamps_inside_scrollport() {
    let mut e = engine(
        r#"<div id="s"><div id="pad"></div><div id="stick">H</div><div id="tall"></div></div>"#,
        "body{margin:0} #s{width:200px;height:100px;overflow:auto} #pad{height:60px} #stick{height:20px;background:#eee;position:sticky;top:10px} #tall{height:500px}",
        400.0,
        300.0,
    );
    let stick = by_id(&e, "stick");
    // Flow position (below the 60px pad): above the stick threshold, so unstuck.
    let y0 = e.element_rect(stick).unwrap()[1];
    assert!(near(y0, 60.0), "unstuck: relative position, got {y0}");
    assert!(e.scroll_by(50.0, 80.0, 0.0, 100.0));
    e.update(0.1);
    let y1 = e.element_rect(stick).unwrap()[1];
    // Content scrolled 100px; the header sticks 10px below the scrollport top.
    let port_top = e.element_rect(by_id(&e, "s")).unwrap()[1];
    assert!(near(y1, port_top + 10.0), "stuck at top:10px, got {y1} (port {port_top})");
    assert!(near(e.get_scroll(by_id(&e, "s")).1, 100.0));
}

// ---- fast path ----------------------------------------------------------------------

#[test]
fn unused_features_add_no_markers_or_batches() {
    let e = engine(r#"<div id="a"></div>"#, "body{margin:0} #a{width:40px;height:40px;background:#123456}", 200.0, 200.0);
    assert!(!has_marker(e.display_list(), "filter"));
    assert!(!has_marker(e.display_list(), "backdrop"));
    assert!(!has_marker(e.display_list(), "ellipse"));
    assert!(e.display_list().items.iter().all(|i| !matches!(i, Item::PushFilter(_) | Item::PopFilter | Item::PushBackdrop(_) | Item::PopBackdrop | Item::PushEllipseClip { .. })));
}
