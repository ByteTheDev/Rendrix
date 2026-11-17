use super::*;
use rendrix_css::Cursor;
use rendrix_dom::Document;

fn engine(html: &str, css: &str, w: f32, h: f32) -> Engine {
    let src = format!("<html><head><style>{css}</style></head><body>{html}</body></html>");
    let mut e = Engine::with_document(Document::parse(&src), Box::new(MapAssets::default()));
    e.set_viewport(w, h, 1.0);
    e.update(0.0);
    e
}

fn by_id(e: &Engine, id: &str) -> NodeId {
    e.doc.get_element_by_id(id).unwrap_or_else(|| panic!("no #{id}"))
}

fn rect(e: &Engine, id: &str) -> [f32; 4] {
    e.element_rect(by_id(e, id)).unwrap_or_else(|| panic!("no rect for #{id}"))
}

fn near(a: f32, b: f32) -> bool {
    (a - b).abs() < 0.6
}

#[test]
fn flexbox_layout_from_css() {
    let e = engine(
        r#"<div id="row"><div id="a"></div><div id="b"></div><div id="c"></div></div>"#,
        "body{margin:0} #row{display:flex;width:400px;height:100px;gap:10px;padding:10px} #a{width:80px} #b{flex:1} #c{width:60px;height:30px}",
        800.0,
        600.0,
    );
    let (a, b, c) = (rect(&e, "a"), rect(&e, "b"), rect(&e, "c"));
    assert!(near(a[0], 10.0) && near(a[2], 80.0), "{a:?}");
    assert!(near(b[0], 100.0), "second item after 80 + gap: {b:?}");
    assert!(near(b[2], 400.0 - 80.0 - 60.0 - 20.0), "flex:1 takes the rest of the 400px content box: {b:?}");
    assert!(near(c[0] + c[2], 410.0), "last item ends at the content edge: {c:?}");
    assert!(near(a[3], 100.0), "stretched to the container content height: {a:?}");
    assert!(near(c[3], 30.0));
}

#[test]
fn block_text_wraps_and_sizes_to_content() {
    let e = engine(r#"<p id="p">The quick brown fox jumps over the lazy dog again and again</p>"#, "body{margin:0} p{margin:0;width:120px;font-size:16px}", 800.0, 600.0);
    let p = rect(&e, "p");
    assert!(near(p[2], 120.0));
    assert!(p[3] > 40.0, "wrapped onto several lines: {p:?}");
    let e2 = engine(r#"<p id="p">short</p>"#, "body{margin:0} p{margin:0;font-size:16px}", 800.0, 600.0);
    assert!(rect(&e2, "p")[3] < 24.0, "one line high");
}

#[test]
fn inline_elements_flow_inside_one_paragraph() {
    let e = engine(r#"<p id="p">Hello <b id="b">bold</b> and <i>italic</i> text</p>"#, "body{margin:0} p{margin:0}", 800.0, 600.0);
    let p = rect(&e, "p");
    assert!(p[3] < 24.0, "single line: {p:?}");
    let b = rect(&e, "b");
    assert!(b[2] > 15.0 && b[2] < 60.0 && b[0] > 20.0, "inline <b> has a box inside the line: {b:?}");
    assert!(e.display_list().items.iter().filter(|i| matches!(i, rendrix_gfx::Item::Text(_))).count() == 1, "one text item for the whole paragraph");
}

#[test]
fn margin_padding_border_and_box_sizing() {
    let e = engine(
        r#"<div id="a"></div><div id="b"></div>"#,
        "body{margin:0} #a{width:100px;height:50px;padding:10px;border:5px solid red;margin:20px} #b{box-sizing:border-box;width:100px;height:50px;padding:10px;border:5px solid red}",
        800.0,
        600.0,
    );
    let (a, b) = (rect(&e, "a"), rect(&e, "b"));
    assert!(near(a[0], 20.0) && near(a[1], 20.0) && near(a[2], 130.0) && near(a[3], 80.0), "{a:?}");
    assert!(near(b[2], 100.0) && near(b[3], 50.0), "{b:?}");
    assert!(near(b[1], 20.0 + 80.0 + 20.0), "after a's bottom margin: {b:?}");
}

#[test]
fn cascade_specificity_inheritance_and_variables() {
    let e = engine(
        r#"<div id="p" class="x"><span id="s">t</span></div>"#,
        ":root{--w:70px} .x{color:rgb(10,20,30)} div#p{--h:20px} #s{display:block;width:var(--w);height:var(--h)}",
        800.0,
        600.0,
    );
    let s = by_id(&e, "s");
    assert_eq!(e.computed_value(s, "color"), "rgb(10, 20, 30)", "inherited");
    assert_eq!(e.computed_value(s, "width"), "70px");
    assert_eq!(e.computed_value(s, "height"), "20px");
}

#[test]
fn media_queries_follow_the_viewport() {
    let mut e = engine(r#"<div id="a"></div>"#, "body{margin:0} #a{height:10px;width:100px} @media (max-width: 500px){#a{width:50px}}", 800.0, 600.0);
    assert!(near(rect(&e, "a")[2], 100.0));
    e.set_viewport(400.0, 600.0, 1.0);
    e.update(0.1);
    assert!(near(rect(&e, "a")[2], 50.0));
}

#[test]
fn dom_mutation_updates_layout() {
    let mut e = engine(r#"<div id="box"></div>"#, "body{margin:0} #box{width:100px;height:40px;background:#123}", 800.0, 600.0);
    assert!(near(rect(&e, "box")[3], 40.0));
    let b = by_id(&e, "box");
    e.doc.set_attr(b, "style", "height: 90px");
    e.update(0.1);
    assert!(near(rect(&e, "box")[3], 90.0));
    let child = e.doc.create_element("div");
    e.doc.set_attr(child, "style", "height:25px;width:10px");
    e.doc.append_child(b, child).unwrap();
    e.update(0.2);
    assert!(near(rect(&e, "box")[3], 90.0), "explicit height unaffected by child");
    e.doc.remove(b);
    e.update(0.3);
    assert!(e.element_rect(b).is_none(), "removed element has no geometry");
}

#[test]
fn hover_state_restyles_and_incremental_restyle_is_scoped() {
    let mut e = engine(r#"<div id="a" class="btn"></div><div id="b" class="btn"></div>"#, ".btn{width:50px;height:20px;background:rgb(0,0,255)} .btn:hover{background:rgb(255,0,0)}", 800.0, 600.0);
    let a = by_id(&e, "a");
    let full_before = e.stats.full_restyles;
    assert_eq!(e.computed_value(a, "background-color"), "rgb(0, 0, 255)");
    e.doc.set_hover_chain(Some(a));
    let info = e.update(0.1);
    assert!(info.repainted);
    assert_eq!(e.computed_value(a, "background-color"), "rgb(255, 0, 0)");
    assert_eq!(e.computed_value(by_id(&e, "b"), "background-color"), "rgb(0, 0, 255)");
    assert_eq!(e.stats.full_restyles, full_before, "hover change did not restyle the whole document");
    e.doc.set_hover_chain(None);
    e.update(0.2);
    assert_eq!(e.computed_value(a, "background-color"), "rgb(0, 0, 255)");
}

#[test]
fn idle_frames_do_no_work() {
    let mut e = engine("<p>hi</p>", "", 300.0, 200.0);
    let frames = e.stats.frames;
    let info = e.update(1.0);
    assert!(!info.repainted && !info.animating);
    assert_eq!(e.stats.frames, frames);
}

#[test]
fn hit_testing_respects_z_order_clip_and_pointer_events() {
    let e = engine(
        r#"<div id="back"></div><div id="front"></div><div id="clipper"><div id="inner"></div></div><div id="ghost"></div>"#,
        "body{margin:0} div{position:absolute;width:100px;height:100px} #back{left:0;top:0;z-index:1} #front{left:50px;top:50px;z-index:2}
         #clipper{left:300px;top:0;width:50px;height:50px;overflow:hidden} #inner{left:0;top:0;width:200px;height:200px}
         #ghost{left:0;top:0;z-index:9;pointer-events:none}",
        800.0,
        600.0,
    );
    assert_eq!(e.hit_test(10.0, 10.0), Some(by_id(&e, "back")), "ghost ignores pointer events");
    assert_eq!(e.hit_test(75.0, 75.0), Some(by_id(&e, "front")), "higher z-index wins in the overlap");
    assert_eq!(e.hit_test(320.0, 20.0), Some(by_id(&e, "inner")));
    assert_eq!(e.hit_test(320.0, 120.0), None, "outside the overflow:hidden clip");
    assert_eq!(e.hit_test(700.0, 500.0), None);
}

#[test]
fn transforms_move_hit_regions() {
    let e = engine(r#"<div id="t"></div>"#, "body{margin:0} #t{width:100px;height:100px;transform:translate(200px, 0) scale(0.5)}", 800.0, 600.0);
    let t = by_id(&e, "t");
    assert_eq!(e.hit_test(250.0, 50.0), Some(t), "scaled about its centre: occupies x 225..275");
    assert_ne!(e.hit_test(50.0, 50.0), Some(t), "the element is no longer at its original position");
    assert_ne!(e.hit_test(205.0, 5.0), Some(t), "outside the scaled box");
    let r = e.element_rect(t).unwrap();
    assert!(near(r[0], 225.0) && near(r[2], 50.0), "{r:?}");
}

#[test]
fn cursor_comes_from_style_and_element_kind() {
    let e = engine(r##"<div id="p" style="cursor:pointer;width:50px;height:50px"></div><input id="i" style="position:absolute;left:100px;top:0"><a id="a" href="#x" style="position:absolute;left:300px;top:0">link</a>"##, "body{margin:0}", 800.0, 600.0);
    assert_eq!(e.cursor_at(10.0, 10.0), Cursor::Pointer);
    assert_eq!(e.cursor_at(110.0, 8.0), Cursor::Text);
    assert_eq!(e.cursor_at(305.0, 8.0), Cursor::Pointer);
    assert_eq!(e.cursor_at(700.0, 500.0), Cursor::Default);
}

#[test]
fn scrolling_moves_content_and_hit_regions() {
    let mut e = engine(r#"<div id="s"><div id="tall"></div></div>"#, "body{margin:0} #s{width:200px;height:100px;overflow:auto} #tall{height:500px;background:#abc}", 800.0, 600.0);
    let s = by_id(&e, "s");
    let (sw, sh, cw, ch) = e.scroll_metrics(s).unwrap();
    assert!(near(sh, 500.0) && near(ch, 100.0) && near(sw, 200.0) && near(cw, 200.0), "{sw} {sh} {cw} {ch}");
    assert!(e.scroll_by(50.0, 50.0, 0.0, 120.0));
    e.update(0.1);
    assert_eq!(e.get_scroll(s), (0.0, 120.0));
    assert!(near(rect(&e, "tall")[1], -120.0));
    assert!(e.scroll_by(50.0, 50.0, 0.0, 10_000.0));
    e.update(0.2);
    assert_eq!(e.get_scroll(s).1, 400.0, "clamped to scrollHeight - clientHeight");
    assert!(!e.scroll_by(50.0, 50.0, 0.0, 10.0), "already at the end");
}

#[test]
fn document_scrolls_when_content_is_taller_than_the_viewport() {
    let mut e = engine(r#"<div id="tall" style="height:2000px"></div>"#, "body{margin:0}", 400.0, 300.0);
    assert!(e.scroll_by(10.0, 10.0, 0.0, 500.0));
    e.update(0.1);
    assert!(near(rect(&e, "tall")[1], -500.0));
}

#[test]
fn transitions_interpolate_and_finish() {
    let mut e = engine(r#"<div id="a" class="c"></div>"#, "body{margin:0} #a{width:100px;height:10px;opacity:1;transition:opacity 1s linear} #a.faded{opacity:0}", 800.0, 600.0);
    let a = by_id(&e, "a");
    e.update(1.0);
    e.doc.toggle_class(a, "faded", Some(true));
    e.update(1.0);
    let at0: f32 = e.computed_value(a, "opacity").parse().unwrap();
    assert!(at0 > 0.95, "starts at the old value: {at0}");
    let info = e.update(1.5);
    assert!(info.animating);
    let mid: f32 = e.computed_value(a, "opacity").parse().unwrap();
    assert!((mid - 0.5).abs() < 0.05, "halfway: {mid}");
    let info = e.update(2.5);
    let end: f32 = e.computed_value(a, "opacity").parse().unwrap();
    assert_eq!(end, 0.0);
    assert!(!info.animating, "no more frames needed after the transition");
}

#[test]
fn keyframe_animation_runs_and_loops() {
    let mut e = engine(r#"<div id="a"></div>"#, "@keyframes grow{from{width:0px}to{width:100px}} body{margin:0} #a{height:10px;animation:grow 2s linear infinite}", 800.0, 600.0);
    let a = by_id(&e, "a");
    e.update(10.0);
    e.update(11.0);
    let w: f32 = e.computed_value(a, "width").trim_end_matches("px").parse().unwrap();
    assert!((w - 50.0).abs() < 2.0, "halfway through the first cycle: {w}");
    let info = e.update(13.0);
    let w: f32 = e.computed_value(a, "width").trim_end_matches("px").parse().unwrap();
    assert!((w - 50.0).abs() < 2.0, "second cycle: {w}");
    assert!(info.animating);
}

#[test]
fn text_input_editing_operations() {
    let mut e = engine(r#"<input id="i" value="hello world">"#, "body{margin:0}", 800.0, 600.0);
    let i = by_id(&e, "i");
    e.doc.set_focus(Some(i));
    e.doc.set_selection(i, 5, 5);
    let r = e.edit(i, EditOp::Insert(",".into()));
    assert!(r.value_changed);
    assert_eq!(e.doc.value(i), "hello, world");
    e.edit(i, EditOp::Backspace { word: false });
    e.edit(i, EditOp::Backspace { word: false });
    assert_eq!(e.doc.value(i), "hell world");
    e.edit(i, EditOp::Home { shift: false });
    e.edit(i, EditOp::Right { shift: true, word: true });
    e.edit(i, EditOp::Insert("X".into()));
    assert_eq!(e.doc.value(i), "Xworld");
    e.edit(i, EditOp::SelectAll);
    e.edit(i, EditOp::Insert("a\nb".into()));
    assert_eq!(e.doc.value(i), "a b", "single-line input flattens newlines");
    e.doc.set_form_value(i, "héllo");
    e.doc.set_selection(i, 5, 5);
    e.edit(i, EditOp::Left { shift: false, word: false });
    e.edit(i, EditOp::Delete { word: false });
    assert_eq!(e.doc.value(i), "hélo", "multi-byte characters stay intact");
    e.doc.set_attr(i, "maxlength", "3");
    e.edit(i, EditOp::SelectAll);
    e.edit(i, EditOp::Insert("abcdef".into()));
    assert_eq!(e.doc.value(i), "abc");
}

#[test]
fn text_input_undo_redo_restores_snapshots() {
    let mut e = engine(r#"<input id="i" value="">"#, "body{margin:0}", 800.0, 600.0);
    let i = by_id(&e, "i");
    e.doc.set_focus(Some(i));
    assert!(!e.edit(i, EditOp::Undo).value_changed, "empty undo stack is a no-op");
    e.edit(i, EditOp::Insert("hello".into()));
    e.edit(i, EditOp::Insert(" world".into()));
    assert_eq!(e.doc.value(i), "hello world");
    assert!(e.edit(i, EditOp::Undo).value_changed);
    assert_eq!(e.doc.value(i), "hello");
    assert!(e.edit(i, EditOp::Undo).value_changed);
    assert_eq!(e.doc.value(i), "");
    assert!(!e.edit(i, EditOp::Undo).value_changed, "stack exhausted");
    assert!(e.edit(i, EditOp::Redo).value_changed);
    assert_eq!(e.doc.value(i), "hello");
    assert!(e.edit(i, EditOp::Redo).value_changed);
    assert_eq!(e.doc.value(i), "hello world");
    assert!(!e.edit(i, EditOp::Redo).value_changed, "redo stack exhausted");
    // A new edit clears the redo stack.
    e.edit(i, EditOp::Undo);
    e.edit(i, EditOp::Insert("!".into()));
    assert_eq!(e.doc.value(i), "hello!");
    assert!(!e.edit(i, EditOp::Redo).value_changed);
}

#[test]
fn textarea_keeps_newlines_and_moves_vertically() {
    let mut e = engine(r#"<textarea id="t" style="width:200px;height:80px"></textarea>"#, "body{margin:0}", 800.0, 600.0);
    let t = by_id(&e, "t");
    e.doc.set_focus(Some(t));
    e.edit(t, EditOp::Insert("one\ntwo\nthree".into()));
    assert_eq!(e.doc.value(t), "one\ntwo\nthree");
    e.update(0.1);
    e.edit(t, EditOp::Up { shift: false });
    let caret = e.doc.element(t).unwrap().form.selection_end;
    assert!((4..=7).contains(&caret), "caret moved onto the second line: {caret}");
    e.edit(t, EditOp::Home { shift: false });
    assert_eq!(e.doc.element(t).unwrap().form.selection_end, 4);
}

#[test]
fn display_list_contains_backgrounds_borders_radius_and_text() {
    use rendrix_gfx::{Fill, Item};
    let e = engine(
        r#"<div id="card">Title</div>"#,
        "body{margin:0;background:#101010} #card{width:200px;height:80px;background:#ff0000;border:2px solid #00ff00;border-radius:12px;color:#fff;box-shadow:0 4px 12px #000}",
        400.0,
        300.0,
    );
    let list = e.display_list();
    assert_eq!(list.background, Color::from_u8(16, 16, 16, 1.0), "body background propagates to the canvas");
    let rects: Vec<_> = list.items.iter().filter_map(|i| if let Item::Rect(r) = i { Some(r) } else { None }).collect();
    assert_eq!(rects.len(), 1, "body background is not painted twice");
    assert!(matches!(rects[0].fill, Fill::Solid(c) if c == Color::from_u8(255, 0, 0, 1.0)));
    assert_eq!(rects[0].radii, [12.0; 4]);
    assert_eq!(rects[0].border.as_ref().unwrap().widths, [2.0; 4]);
    assert!(list.items.iter().any(|i| matches!(i, Item::Shadow(_))));
    assert!(list.items.iter().any(|i| matches!(i, Item::Text(_))));
}

#[test]
fn per_side_border_colors_and_styles_reach_the_display_list() {
    use rendrix_gfx::Item;
    let e = engine(
        r#"<div id="d">x</div>"#,
        "body{margin:0} #d{width:40px;height:20px;border-width:2px 3px 4px 5px;border-style:solid dashed dotted double;border-color:red green blue yellow}",
        400.0,
        300.0,
    );
    let rects: Vec<_> = e
        .display_list()
        .items
        .iter()
        .filter_map(|i| if let Item::Rect(r) = i { Some(r) } else { None })
        .collect();
    let b = rects.iter().find_map(|r| r.border.as_ref()).expect("border in display list");
    assert_eq!(b.widths, [2.0, 3.0, 4.0, 5.0]);
    assert_eq!(b.colors[0], Color::from_u8(255, 0, 0, 1.0));
    assert_eq!(b.colors[1], Color::from_u8(0, 128, 0, 1.0));
    assert_eq!(b.colors[2], Color::from_u8(0, 0, 255, 1.0));
    assert_eq!(b.colors[3], Color::from_u8(255, 255, 0, 1.0));
    assert_eq!(b.styles, [0, 1, 2, 3]);
}

#[test]
fn generated_content_and_lists() {
    let e = engine(r#"<ul><li id="l1">one</li><li>two</li></ul><span id="q" class="q">x</span>"#, "body{margin:0} .q::before{content:'['} .q::after{content:']'}", 400.0, 300.0);
    let texts: Vec<String> = e
        .display_list()
        .items
        .iter()
        .filter_map(|i| if let rendrix_gfx::Item::Text(t) = i { Some(t.content.runs.iter().map(|r| r.text.clone()).collect::<String>()) } else { None })
        .collect();
    assert!(texts.iter().any(|t| t == "\u{2022} one"), "{texts:?}");
    assert!(texts.iter().any(|t| t == "[x]"), "{texts:?}");
}

#[test]
fn buttons_and_atomic_inlines_shrink_wrap_and_flow() {
    let e = engine(r#"<div id="d"><button id="b1">One</button> <button id="b2">Two</button></div>"#, "body{margin:0}", 800.0, 600.0);
    let (b1, b2, d) = (rect(&e, "b1"), rect(&e, "b2"), rect(&e, "d"));
    assert!(b1[2] > 20.0 && b1[2] < 100.0, "button is shrink-to-fit, not full width: {b1:?}");
    assert!(b2[0] > b1[0] + b1[2], "second button flows to the right: {b1:?} {b2:?}");
    assert!(near(b1[1], b2[1]));
    assert!(d[3] >= b1[3]);
}

#[test]
fn absolute_children_use_the_nearest_positioned_ancestor() {
    let e = engine(
        r#"<div id="host"><div id="mid"><div id="abs"></div></div></div>"#,
        "body{margin:0} #host{position:relative;margin:40px;width:300px;height:200px} #mid{padding:30px} #abs{position:absolute;right:10px;bottom:10px;width:50px;height:50px}",
        800.0,
        600.0,
    );
    let a = rect(&e, "abs");
    assert!(near(a[0], 40.0 + 300.0 - 10.0 - 50.0) && near(a[1], 40.0 + 200.0 - 10.0 - 50.0), "{a:?}");
}

#[test]
fn images_load_from_assets_and_size_boxes() {
    let mut png = Vec::new();
    {
        let img = image::RgbaImage::from_pixel(40, 20, image::Rgba([255, 0, 0, 255]));
        image::DynamicImage::ImageRgba8(img).write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png).unwrap();
    }
    let mut assets = MapAssets::default();
    assets.0.insert("pic.png".into(), png);
    let src = r#"<html><body style="margin:0"><img id="i" src="pic.png"><img id="j" src="pic.png" style="width:80px"><img id="k" src="missing.png" width="10" height="10"></body></html>"#;
    let mut e = Engine::with_document(Document::parse(src), Box::new(assets));
    e.set_viewport(400.0, 300.0, 1.0);
    e.update(0.0);
    let (i, j) = (rect(&e, "i"), rect(&e, "j"));
    assert!(near(i[2], 40.0) && near(i[3], 20.0), "natural size: {i:?}");
    assert!(near(j[2], 80.0) && near(j[3], 40.0), "aspect ratio preserved: {j:?}");
    assert!(e.display_list().items.iter().any(|it| matches!(it, rendrix_gfx::Item::Image(_))));
    assert_eq!(e.take_image_uploads().len(), 1, "decoded once, shared by both elements");
}

#[test]
fn grid_layout_from_css() {
    let e = engine(
        r#"<div id="g"><div id="a"></div><div id="b"></div><div id="c"></div></div>"#,
        "body{margin:0} #g{display:grid;grid-template-columns:100px 1fr;gap:10px;width:310px} #g>div{height:20px} #c{grid-column:1 / span 2}",
        800.0,
        600.0,
    );
    let (a, b, c) = (rect(&e, "a"), rect(&e, "b"), rect(&e, "c"));
    assert!(near(a[2], 100.0) && near(b[0], 110.0) && near(b[2], 200.0), "{a:?} {b:?}");
    assert!(near(c[1], 30.0) && near(c[2], 310.0), "spans both columns on the next row: {c:?}");
}

#[test]
fn moved_nodes_pick_up_inherited_style_from_their_new_parent() {
    let mut e = engine(r#"<div id="red" style="color:rgb(255,0,0)"><span id="s">x</span></div><div id="blue" style="color:rgb(0,0,255)"></div>"#, "", 400.0, 300.0);
    let (s, blue) = (by_id(&e, "s"), by_id(&e, "blue"));
    assert_eq!(e.computed_value(s, "color"), "rgb(255, 0, 0)");
    e.doc.append_child(blue, s).unwrap();
    e.update(0.1);
    assert_eq!(e.computed_value(s, "color"), "rgb(0, 0, 255)", "stale cached style must not survive a re-parent");
}

#[test]
fn structural_selectors_restyle_siblings_on_insert_and_remove() {
    let mut e = engine("<ul id=u><li id=a>1</li><li id=b>2</li></ul>", "li{height:10px} li:first-child{color:rgb(1,1,1)} li:last-child{color:rgb(2,2,2)}", 400.0, 300.0);
    let (u, a, b) = (by_id(&e, "u"), by_id(&e, "a"), by_id(&e, "b"));
    assert_eq!((e.computed_value(a, "color"), e.computed_value(b, "color")), ("rgb(1, 1, 1)".into(), "rgb(2, 2, 2)".into()));
    let c = e.doc.create_element("li");
    e.doc.append_child(u, c).unwrap();
    e.update(0.1);
    assert_eq!(e.computed_value(b, "color"), "rgb(0, 0, 0)", "b is no longer last-child");
    assert_eq!(e.computed_value(c, "color"), "rgb(2, 2, 2)", "the new node is styled as last-child");
    e.doc.remove(a);
    e.update(0.2);
    assert_eq!(e.computed_value(b, "color"), "rgb(1, 1, 1)", "b became first-child");
}

#[test]
fn child_list_changes_do_not_restyle_unrelated_subtrees_without_structural_rules() {
    let mut e = engine("<div id=p><div id=k1 class=k></div><div id=k2 class=k></div></div>", ".k{height:5px}", 400.0, 300.0);
    let p = by_id(&e, "p");
    let before = e.stats.full_restyles;
    let n = e.doc.create_element("div");
    e.doc.set_attr(n, "class", "k");
    e.doc.append_child(p, n).unwrap();
    e.update(0.1);
    assert_eq!(e.stats.full_restyles, before);
    assert!(rect(&e, "k2")[3] > 0.0 && e.element_rect(n).is_some(), "the new node is styled and laid out");
}

#[test]
fn non_local_hover_rules_still_reach_descendants() {
    let mut e = engine("<div id=card class=card><span id=t>x</span></div>", ".card .t, .card:hover #t{color:rgb(9,9,9)} .card:hover #t{color:rgb(200,0,0)}", 400.0, 300.0);
    let (card, t) = (by_id(&e, "card"), by_id(&e, "t"));
    assert_eq!(e.computed_value(t, "color"), "rgb(0, 0, 0)");
    e.doc.set_hover_chain(Some(card));
    e.update(0.1);
    assert_eq!(e.computed_value(t, "color"), "rgb(200, 0, 0)", "ancestor :hover affects a descendant");
    e.doc.set_hover_chain(None);
    e.update(0.2);
    assert_eq!(e.computed_value(t, "color"), "rgb(0, 0, 0)");
}

#[test]
fn offscreen_content_is_culled_but_visible_content_and_hits_survive() {
    use rendrix_gfx::Item;
    let mut e = engine(r#"<div id="list"></div>"#, "body{margin:0} .row{height:20px;background:#333}", 300.0, 100.0);
    let list = by_id(&e, "list");
    for i in 0..500 {
        let d = e.doc.create_element("div");
        e.doc.set_attr(d, "class", "row");
        e.doc.set_attr(d, "id", &format!("r{i}"));
        e.doc.append_child(list, d).unwrap();
    }
    e.update(0.1);
    let rects = e.display_list().items.iter().filter(|i| matches!(i, Item::Rect(_))).count();
    assert!(rects <= 8, "only the ~5 visible rows are emitted, got {rects}");
    assert_eq!(e.hit_test(10.0, 45.0), Some(by_id(&e, "r2")));
    assert!(e.scroll_by(10.0, 10.0, 0.0, 1000.0));
    e.update(0.2);
    let rects = e.display_list().items.iter().filter(|i| matches!(i, Item::Rect(_))).count();
    assert!((4..=8).contains(&rects), "scrolled window still shows rows: {rects}");
    assert_eq!(e.hit_test(10.0, 5.0), Some(by_id(&e, "r50")));
}

#[test]
fn shadows_and_overflow_keep_partially_offscreen_boxes_alive() {
    use rendrix_gfx::Item;
    let e = engine(r#"<div id="a" style="position:absolute;left:10px;top:-60px;width:50px;height:50px;box-shadow:0 40px 30px #000"></div><div id="p" style="position:absolute;left:0;top:-200px;width:10px;height:10px"><div style="position:absolute;top:210px;left:0;width:20px;height:20px;background:#f00"></div></div>"#, "body{margin:0}", 200.0, 100.0);
    assert!(e.display_list().items.iter().any(|i| matches!(i, Item::Shadow(_))), "a box above the viewport whose shadow reaches in is still painted");
    assert!(e.display_list().items.iter().any(|i| matches!(i, Item::Rect(r) if r.rect[3] == 20.0)), "overflowing child of an offscreen parent is still painted");
}

#[test]
fn scene_hosts_emit_scene3d_items_with_border_box_rects() {
    use rendrix_gfx::Item;
    let e = engine(
        r#"<rendrix-scene id="s"></rendrix-scene><canvas id="c" data-3d width="300" height="150"></canvas><canvas id="plain" width="10" height="10"></canvas><div id="d"></div>"#,
        "body{margin:0} #s{width:200px;height:100px} #c{width:300px;height:150px}",
        800.0,
        600.0,
    );
    let list = e.display_list();
    assert!(list.has_3d, "scene hosts set the 3D flag");
    let scenes: Vec<_> = list.items.iter().filter_map(|i| if let Item::Scene3D(s) = i { Some(s) } else { None }).collect();
    assert_eq!(scenes.len(), 2, "one per scene host, got {}", scenes.len());
    let s = scenes.iter().find(|s| s.scene_id == by_id(&e, "s") as u64).expect("rendrix-scene item");
    assert!(near(s.rect[2], 200.0) && near(s.rect[3], 100.0), "border-box rect: {:?}", s.rect);
    assert!(scenes.iter().any(|s| s.scene_id == by_id(&e, "c") as u64), "canvas data-3d item");
    // The data-3d canvas is taken over by its scene: no 2D bitmap composite for it,
    // while the plain canvas keeps the existing Image path.
    let images: Vec<_> = list.items.iter().filter_map(|i| if let Item::Image(im) = i { Some(im.image) } else { None }).collect();
    assert!(!images.contains(&rendrix_gfx::canvas_image_id(by_id(&e, "c"))), "data-3d canvas emits no Image");
    assert!(images.contains(&rendrix_gfx::canvas_image_id(by_id(&e, "plain"))), "plain canvas keeps the Image path");

    // No 3D element anywhere: identical 2D behavior, flag clear.
    let e2 = engine(r#"<div id="d"></div><canvas id="plain" width="10" height="10"></canvas>"#, "body{margin:0}", 800.0, 600.0);
    assert!(!e2.display_list().has_3d);
    assert!(e2.display_list().items.iter().all(|i| !matches!(i, Item::Scene3D(_))));
}

#[test]
fn text_overflow_ellipsis_truncates_with_ellipsis() {
    use rendrix_gfx::Item;
    let e = engine(
        r#"<div id="a">Hello world this is a long line that must overflow</div>"#,
        "body{margin:0} #a{width:100px;white-space:nowrap;overflow:hidden;text-overflow:ellipsis;font-size:16px}",
        800.0,
        600.0,
    );
    let texts: Vec<String> = e
        .display_list()
        .items
        .iter()
        .filter_map(|i| if let Item::Text(t) = i { Some(t.content.runs.iter().map(|r| r.text.clone()).collect::<String>()) } else { None })
        .collect();
    assert!(!texts.is_empty(), "text is painted");
    let shown = texts.join("");
    assert!(shown.contains('…'), "truncated text ends with ellipsis: {shown:?}");
    assert!(!shown.contains("overflow"), "overflowing tail is cut: {shown:?}");
    // Without the flag the full text survives (no ellipsis).
    let full = engine(
        r#"<div id="a">Hello world this is a long line that must overflow</div>"#,
        "body{margin:0} #a{width:100px;white-space:nowrap;overflow:hidden;font-size:16px}",
        800.0,
        600.0,
    );
    let full_text: String = full
        .display_list()
        .items
        .iter()
        .filter_map(|i| if let Item::Text(t) = i { Some(t.content.runs.iter().map(|r| r.text.clone()).collect::<String>()) } else { None })
        .collect();
    assert!(full_text.contains("overflow") && !full_text.contains('…'), "no truncation without text-overflow:ellipsis: {full_text:?}");
}

#[test]
fn line_clamp_limits_visible_lines_with_ellipsis() {
    use rendrix_gfx::Item;
    let long = "alpha beta gamma delta epsilon zeta eta theta iota kappa lambda mu nu xi omicron pi rho sigma tau";
    let html = format!(r#"<div id="a">{long}</div>"#);
    let e = engine(
        &html,
        "body{margin:0} #a{width:80px;font-size:16px;line-height:20px;overflow:hidden;-webkit-line-clamp:2}",
        800.0,
        600.0,
    );
    let shown: String = e
        .display_list()
        .items
        .iter()
        .filter_map(|i| if let Item::Text(t) = i { Some(t.content.runs.iter().map(|r| r.text.clone()).collect::<String>()) } else { None })
        .collect();
    assert!(shown.contains('…'), "clamped text ends with ellipsis: {shown:?}");
    assert!(shown.len() < long.len(), "clamped text is shorter than the source: {shown:?}");
    // Without line-clamp the full text is painted.
    let full = engine(
        &html,
        "body{margin:0} #a{width:80px;font-size:16px;line-height:20px;overflow:hidden}",
        800.0,
        600.0,
    );
    let full_text: String = full
        .display_list()
        .items
        .iter()
        .filter_map(|i| if let Item::Text(t) = i { Some(t.content.runs.iter().map(|r| r.text.clone()).collect::<String>()) } else { None })
        .collect();
    assert_eq!(full_text, long, "no clamp keeps all text: {full_text:?}");
}

#[test]
fn container_query_uses_the_containers_box_size() {
    let mut e = engine(
        r#"<div id="box"><span id="child">x</span></div>"#,
        "body{margin:0} #box{width:200px;height:40px;container-type:inline-size;container-name:card} #child{color:red} \
         @container card (min-width:180px){#child{color:lime}} \
         @container card (min-width:400px){#child{background:blue}}",
        400.0,
        300.0,
    );
    // First frame lays out the container; its size feeds the next restyle.
    assert_eq!(e.computed_value(by_id(&e, "child"), "color"), "rgb(255, 0, 0)");
    e.update(1.0);
    assert_eq!(e.computed_value(by_id(&e, "child"), "color"), "rgb(0, 255, 0)");
    assert_eq!(e.computed_value(by_id(&e, "child"), "background-color"), "rgba(0, 0, 0, 0)");
}

#[test]
fn tables_collapse_to_a_grid() {
    let e = engine(
        r#"<table id="t"><tr><td id="a">a</td><td id="b">b</td></tr><tr><td id="c">c</td><td id="d">d</td></tr></table>"#,
        "body{margin:0} #t{width:300px} td{height:20px}",
        800.0,
        600.0,
    );
    let (a, b, c, d) = (rect(&e, "a"), rect(&e, "b"), rect(&e, "c"), rect(&e, "d"));
    assert!(near(a[0], 0.0) && near(a[2], 150.0), "first cell, first column: {a:?}");
    assert!(near(b[0], 150.0) && near(b[2], 150.0), "second cell shares the row: {b:?}");
    assert!(near(b[1], a[1]), "same row: {a:?} {b:?}");
    assert!(near(c[0], 0.0) && near(c[1], 20.0), "second row below the first: {c:?}");
    assert!(near(d[0], 150.0) && near(d[1], 20.0), "{d:?}");
}

#[test]
fn table_sections_caption_and_columns() {
    let e = engine(
        r#"<table id="t"><caption id="cap">cap</caption><colgroup><col><col></colgroup><thead><tr><td id="h">h</td><td>h2</td></tr></thead><tbody><tr><td id="a">a</td><td id="b">b</td></tr></tbody></table>"#,
        "body{margin:0} #t{width:200px} td{height:10px}",
        800.0,
        600.0,
    );
    let (cap, h, a, b) = (rect(&e, "cap"), rect(&e, "h"), rect(&e, "a"), rect(&e, "b"));
    assert!(near(cap[0], 0.0) && near(cap[2], 200.0), "caption spans the table width: {cap:?}");
    assert!(near(h[1], cap[1] + cap[3]), "header row below the caption: {h:?} {cap:?}");
    assert!(near(a[1], b[1]), "body cells share a row: {a:?} {b:?}");
    assert!(near(a[2], 100.0) && near(b[2], 100.0) && near(b[0], 100.0), "two equal columns: {a:?} {b:?}");
    assert!(a[1] > h[1], "tbody below thead: {a:?} {h:?}");
}

#[test]
fn explicit_table_display_values_become_grid_like() {
    // Author-specified table roles (not just UA defaults for <table>/<tr>/<td>).
    let e = engine(
        r#"<div id="t"><div id="r"><div id="a">a</div><div id="b">b</div></div></div>"#,
        "body{margin:0} #t{display:table;width:200px} #r{display:table-row} #a,#b{display:table-cell;height:10px}",
        800.0,
        600.0,
    );
    let (a, b) = (rect(&e, "a"), rect(&e, "b"));
    assert!(near(a[2], 100.0) && near(b[0], 100.0) && near(b[1], a[1]), "{a:?} {b:?}");
}

#[test]
fn floats_pin_to_the_content_edges() {
    let e = engine(
        r#"<div id="wrap"><div id="fl">f</div><div id="fr">g</div></div>"#,
        "body{margin:0} #wrap{width:300px} #fl{float:left;width:100px;height:40px} #fr{float:right;width:120px;height:30px}",
        800.0,
        600.0,
    );
    let (fl, fr) = (rect(&e, "fl"), rect(&e, "fr"));
    assert!(near(fl[0], 0.0) && near(fl[2], 100.0), "left float at the inline start: {fl:?}");
    assert!(near(fr[0], 180.0) && near(fr[2], 120.0), "right float at the inline end: {fr:?}");
    assert_eq!(e.computed_value(by_id(&e, "fl"), "float"), "left");
    assert_eq!(e.computed_value(by_id(&e, "fr"), "float"), "right");
}

#[test]
fn clear_pushes_boxes_below_preceding_floats() {
    let e = engine(
        r#"<div id="wrap"><div id="fl">f</div><div id="pulled">p</div></div>"#,
        "body{margin:0} #wrap{width:300px} #fl{float:left;width:100px;height:60px} #pulled{clear:left;height:20px;margin-top:-40px}",
        800.0,
        600.0,
    );
    let (fl, p) = (rect(&e, "fl"), rect(&e, "pulled"));
    assert!(near(fl[3], 60.0));
    assert!(near(p[1], 60.0), "clear:left starts below the float bottom: {p:?}");
    assert_eq!(e.computed_value(by_id(&e, "pulled"), "clear"), "left");
}

#[test]
fn multicol_splits_children_into_equal_tracks() {
    let e = engine(
        r#"<div id="mc"><div id="a">a</div><div id="b">b</div><div id="c">c</div><div id="d">d</div></div>"#,
        "body{margin:0} #mc{column-count:2;width:300px} #mc>div{height:10px}",
        800.0,
        600.0,
    );
    let (a, b, c, d) = (rect(&e, "a"), rect(&e, "b"), rect(&e, "c"), rect(&e, "d"));
    assert!(near(a[0], 0.0) && near(a[2], 150.0), "{a:?}");
    assert!(near(b[0], 150.0) && near(b[2], 150.0) && near(b[1], a[1]), "{b:?}");
    assert!(near(c[0], 0.0) && near(c[1], 10.0), "{c:?}");
    assert!(near(d[0], 150.0) && near(d[1], 10.0), "{d:?}");
    assert_eq!(e.computed_value(by_id(&e, "mc"), "column-count"), "2");
}

#[test]
fn multicol_column_width_derives_a_count_and_span_all_stretches() {
    let e = engine(
        r#"<div id="mc"><div id="a">a</div><div id="s">s</div><div id="b">b</div></div>"#,
        "body{margin:0} #mc{column-width:200px;width:800px} #mc>div{height:10px} #s{column-span:all}",
        800.0,
        600.0,
    );
    let (a, s) = (rect(&e, "a"), rect(&e, "s"));
    assert!(near(a[2], 200.0), "800px / 200px columns: {a:?}");
    assert!(near(s[2], 800.0), "column-span:all stretches across every track: {s:?}");
}
