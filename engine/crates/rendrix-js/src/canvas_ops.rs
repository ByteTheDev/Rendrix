//! Native Canvas 2D operations (A3, approved subset).
//!
//! This module owns **all** canvas native code so `host.rs` stays untouched: the main agent
//! wires one dispatch arm (see below) plus `pub mod canvas_ops;` in `lib.rs`. Canvas bitmaps
//! live in `rendrix_gfx::canvas`, keyed by hosting document, so every window keeps its own
//! pixels with no changes to `DomHost` or `Engine`.
//!
//! ## Wiring snippet for `host.rs` (`DomHost::call`, before the `other =>` arm)
//! ```ignore
//! op if op.starts_with("canvas_") => match crate::canvas_ops::canvas_op(self, op, a) {
//!     Some(r) => r?,
//!     None => return Err(format!("unknown native operation '{op}'")),
//! },
//! ```
//! ## Wiring snippet for `lib.rs`
//! ```ignore
//! pub mod canvas_ops;
//! ```
//! ## Wiring snippet for the frame shell (`window.rs` redraw + `headless.rs` screenshot,
//! ## after the `take_image_uploads` loop)
//! ```ignore
//! for up in rendrix_js::canvas_ops::take_canvas_uploads(&engine) {
//!     if up.rgba.is_empty() { state.renderer.remove_canvas(up.node); continue; }
//!     match up.dirty {
//!         // Dirty-region re-upload when cheap, full upload otherwise.
//!         Some(r) if r[2] * r[3] * 2 < up.width * up.height => {
//!             state.renderer.update_canvas_region(up.node, &up.rgba, up.width, up.height, r)
//!         }
//!         _ => state.renderer.upload_canvas(up.node, &up.rgba, up.width, up.height),
//!     }
//! }
//! ```

use crate::engine_trait::Val;
use crate::host::{base64_decode, base64_encode, DomHost};
use rendrix_gfx::canvas::{self, Canvas2D, LineCap, LineJoin, LinearGradient, Paint};

/// Route a `canvas_*` op. Returns `None` when `op` is not a canvas op (the host falls through
/// to its own arms); `Some(Err)` surfaces through `mut()` as a `DOMException`.
pub fn canvas_op(host: &DomHost, op: &str, args: &[Val]) -> Option<Result<Val, String>> {
    if !op.starts_with("canvas_") {
        return None;
    }
    Some(run(host, op, args))
}

fn run(host: &DomHost, op: &str, args: &[Val]) -> Result<Val, String> {
    match op {
        "canvas_ensure" => {
            let n = node(args, 0)?;
            ensure(host, n);
            Ok(Val::Undefined)
        }
        "canvas_resize" => {
            let n = node(args, 0)?;
            let (w, h) = (
                attr_dim(host, n, "width", 300.0),
                attr_dim(host, n, "height", 150.0),
            );
            with(host, n, w, h, |c| c.resize(w, h))?;
            touch(host, n);
            Ok(Val::Undefined)
        }
        "canvas_save" => {
            let n = node(args, 0)?;
            with(host, n, 0, 0, |c| c.save())?;
            Ok(Val::Undefined)
        }
        "canvas_restore" => {
            let n = node(args, 0)?;
            with(host, n, 0, 0, |c| c.restore())?;
            Ok(Val::Undefined)
        }
        "canvas_set_transform" => {
            let n = node(args, 0)?;
            with(host, n, 0, 0, |c| c.set_transform(m6(args, 1)))?;
            Ok(Val::Undefined)
        }
        "canvas_transform" => {
            let n = node(args, 0)?;
            with(host, n, 0, 0, |c| c.transform(m6(args, 1)))?;
            Ok(Val::Undefined)
        }
        "canvas_begin" => {
            let n = node(args, 0)?;
            with(host, n, 0, 0, |c| c.begin_path())?;
            Ok(Val::Undefined)
        }
        "canvas_move_to" => {
            let n = node(args, 0)?;
            with(host, n, 0, 0, |c| c.move_to(num(args, 1), num(args, 2)))?;
            Ok(Val::Undefined)
        }
        "canvas_line_to" => {
            let n = node(args, 0)?;
            with(host, n, 0, 0, |c| c.line_to(num(args, 1), num(args, 2)))?;
            Ok(Val::Undefined)
        }
        "canvas_rect" => {
            let n = node(args, 0)?;
            with(host, n, 0, 0, |c| {
                c.rect(num(args, 1), num(args, 2), num(args, 3), num(args, 4))
            })?;
            Ok(Val::Undefined)
        }
        "canvas_arc" => {
            let n = node(args, 0)?;
            with(host, n, 0, 0, |c| {
                c.arc(
                    num(args, 1),
                    num(args, 2),
                    num(args, 3),
                    num(args, 4),
                    num(args, 5),
                    flag(args, 6),
                )
            })??;
            Ok(Val::Undefined)
        }
        "canvas_ellipse" => {
            let n = node(args, 0)?;
            with(host, n, 0, 0, |c| {
                c.ellipse(
                    num(args, 1),
                    num(args, 2),
                    num(args, 3),
                    num(args, 4),
                    num(args, 5),
                    num(args, 6),
                    num(args, 7),
                    flag(args, 8),
                )
            })??;
            Ok(Val::Undefined)
        }
        "canvas_close" => {
            let n = node(args, 0)?;
            with(host, n, 0, 0, |c| c.close_path())?;
            Ok(Val::Undefined)
        }
        "canvas_fill" => {
            let n = node(args, 0)?;
            with(host, n, 0, 0, |c| c.fill())?;
            touch(host, n);
            Ok(Val::Undefined)
        }
        "canvas_stroke" => {
            let n = node(args, 0)?;
            with(host, n, 0, 0, |c| c.stroke())?;
            touch(host, n);
            Ok(Val::Undefined)
        }
        "canvas_fill_rect" => {
            let n = node(args, 0)?;
            with(host, n, 0, 0, |c| {
                c.fill_rect(num(args, 1), num(args, 2), num(args, 3), num(args, 4))
            })?;
            touch(host, n);
            Ok(Val::Undefined)
        }
        "canvas_stroke_rect" => {
            let n = node(args, 0)?;
            with(host, n, 0, 0, |c| {
                c.stroke_rect(num(args, 1), num(args, 2), num(args, 3), num(args, 4))
            })?;
            touch(host, n);
            Ok(Val::Undefined)
        }
        "canvas_clear_rect" => {
            let n = node(args, 0)?;
            with(host, n, 0, 0, |c| {
                c.clear_rect(num(args, 1), num(args, 2), num(args, 3), num(args, 4))
            })?;
            touch(host, n);
            Ok(Val::Undefined)
        }
        "canvas_set_fill" => {
            let n = node(args, 0)?;
            let p = paint_of(str_of(args, 1))?;
            with(host, n, 0, 0, |c| c.set_fill(p))?;
            Ok(Val::Undefined)
        }
        "canvas_set_stroke" => {
            let n = node(args, 0)?;
            let p = paint_of(str_of(args, 1))?;
            with(host, n, 0, 0, |c| c.set_stroke(p))?;
            Ok(Val::Undefined)
        }
        "canvas_set_line_width" => {
            let n = node(args, 0)?;
            with(host, n, 0, 0, |c| c.set_line_width(num(args, 1)))?;
            Ok(Val::Undefined)
        }
        "canvas_set_line_cap" => {
            let n = node(args, 0)?;
            let cap = match str_of(args, 1).as_str() {
                "round" => LineCap::Round,
                "square" => LineCap::Square,
                _ => LineCap::Butt,
            };
            with(host, n, 0, 0, |c| c.set_line_cap(cap))?;
            Ok(Val::Undefined)
        }
        "canvas_set_line_join" => {
            let n = node(args, 0)?;
            let join = match str_of(args, 1).as_str() {
                "round" => LineJoin::Round,
                "bevel" => LineJoin::Bevel,
                _ => LineJoin::Miter,
            };
            with(host, n, 0, 0, |c| c.set_line_join(join))?;
            Ok(Val::Undefined)
        }
        "canvas_set_alpha" => {
            let n = node(args, 0)?;
            with(host, n, 0, 0, |c| c.set_alpha(num(args, 1)))?;
            Ok(Val::Undefined)
        }
        "canvas_draw_image" => {
            let n = node(args, 0)?;
            draw_image(host, n, &str_of(args, 1), args)?;
            touch(host, n);
            Ok(Val::Undefined)
        }
        "canvas_get_data" => {
            let n = node(args, 0)?;
            let (x, y, w, h) = (
                num(args, 1) as i32,
                num(args, 2) as i32,
                num(args, 3),
                num(args, 4),
            );
            if w <= 0.0 || h <= 0.0 {
                return Err("IndexSizeError: getImageData dimensions must be positive".to_string());
            }
            let (w, h) = (w as u32, h as u32);
            let data = canvas::read_canvas(doc_key(host), n, |c| {
                c.map(|c| c.get_data(x, y, w, h))
                    .unwrap_or_else(|| vec![0u8; (w * h * 4) as usize])
            });
            Ok(Val::Str(base64_encode(&data)))
        }
        "canvas_put_data" => {
            let n = node(args, 0)?;
            let data = base64_decode(&str_of(args, 1)).ok_or_else(|| {
                "EncodingError: putImageData payload is not valid base64".to_string()
            })?;
            let (w, h) = (num(args, 2) as u32, num(args, 3) as u32);
            let (dx, dy) = (num(args, 4) as i32, num(args, 5) as i32);
            if data.len() < (w * h * 4) as usize {
                return Err(
                    "IndexSizeError: putImageData payload is smaller than width*height*4"
                        .to_string(),
                );
            }
            with(host, n, 0, 0, |c| c.put_data(&data, w, h, dx, dy))?;
            touch(host, n);
            Ok(Val::Undefined)
        }
        "canvas_data_url" => {
            let n = node(args, 0)?;
            let url = canvas::read_canvas(doc_key(host), n, |c| {
                match c.and_then(|c| c.to_png_bytes()) {
                    Some(png) => format!("data:image/png;base64,{}", base64_encode(&png)),
                    None => "data:,".to_string(),
                }
            });
            Ok(Val::Str(url))
        }
        _ => Err(format!("unknown native operation '{op}'")),
    }
}

fn node(args: &[Val], i: usize) -> Result<u32, String> {
    args.get(i)
        .and_then(|v| v.as_f64())
        .map(|n| n as u32)
        .ok_or_else(|| format!("TypeError: canvas op argument {i} must be a node id"))
}

fn num(args: &[Val], i: usize) -> f32 {
    args.get(i).and_then(|v| v.as_f64()).unwrap_or(0.0) as f32
}

fn flag(args: &[Val], i: usize) -> bool {
    args.get(i).is_some_and(|v| v.as_bool())
}

fn str_of(args: &[Val], i: usize) -> String {
    args.get(i)
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string()
}

fn m6(args: &[Val], i: usize) -> [f32; 6] {
    [
        num(args, i),
        num(args, i + 1),
        num(args, i + 2),
        num(args, i + 3),
        num(args, i + 4),
        num(args, i + 5),
    ]
}

/// The hosting document's address: stable for the engine's lifetime, identical from `DomHost`
/// and from `paint.rs`, so bitmaps and the pending set never cross windows.
pub fn doc_key(host: &DomHost) -> usize {
    (&host.engine.borrow().doc as *const rendrix_dom::Document) as usize
}

/// Attribute size: `width`/`height` content attributes, defaults 300×150, negatives clamp to 0,
/// anything huge clamps to `MAX_CANVAS_DIM` (the attribute itself is untouched).
fn attr_dim(host: &DomHost, n: u32, name: &str, def: f32) -> u32 {
    let e = host.engine.borrow();
    let v = e
        .doc
        .get_attr(n, name)
        .and_then(|s| s.trim().parse::<f64>().ok())
        .unwrap_or(def as f64);
    if !v.is_finite() {
        return def as u32;
    }
    (v.max(0.0).min(canvas::MAX_CANVAS_DIM as f64)) as u32
}

/// Create/reconcile the bitmap (attribute changes via `setAttribute` resize here, per spec the
/// bitmap clears on resize) and prune bitmaps of dead nodes.
fn ensure(host: &DomHost, n: u32) {
    let key = doc_key(host);
    let (w, h) = {
        let e = host.engine.borrow();
        (
            attr_dim_inner(&e, n, "width", 300.0),
            attr_dim_inner(&e, n, "height", 150.0),
        )
    };
    canvas::with_canvas(key, n, w, h, |_| {});
    canvas::prune(key, |m| host.engine.borrow().doc.is_alive(m));
}

fn attr_dim_inner(e: &rendrix_core::Engine, n: u32, name: &str, def: f32) -> u32 {
    let v = e
        .doc
        .get_attr(n, name)
        .and_then(|s| s.trim().parse::<f64>().ok())
        .unwrap_or(def as f64);
    if !v.is_finite() {
        return def as u32;
    }
    (v.max(0.0).min(canvas::MAX_CANVAS_DIM as f64)) as u32
}

/// Run `f` on an existing (reconciled) bitmap; missing canvas elements are an error.
fn with<R>(
    host: &DomHost,
    n: u32,
    _w: u32,
    _h: u32,
    f: impl FnOnce(&mut Canvas2D) -> R,
) -> Result<R, String> {
    let key = doc_key(host);
    {
        let e = host.engine.borrow();
        if !e.doc.is_alive(n) || e.doc.tag(n) != Some("canvas") {
            return Err("InvalidStateError: canvas element is not in the document".to_string());
        }
        let (w, h) = (
            attr_dim_inner(&e, n, "width", 300.0),
            attr_dim_inner(&e, n, "height", 150.0),
        );
        drop(e);
        Ok(canvas::with_canvas(key, n, w, h, f))
    }
}

/// Invalidation for a canvas draw: only the canvas element is marked (pending set), plus the
/// global `paint` flag so `Engine::update` repaints **without** restyle or relayout.
fn touch(host: &DomHost, n: u32) {
    let key = doc_key(host);
    canvas::mark_dirty(key, n);
    host.engine.borrow_mut().doc.dirty.paint = true;
}

fn paint_of(json: String) -> Result<Paint, String> {
    let v: serde_json::Value =
        serde_json::from_str(&json).map_err(|_| "SyntaxError: invalid paint style".to_string())?;
    if let Some(c) = v.get("color").and_then(|c| c.as_str()) {
        return canvas::parse_color(c)
            .map(Paint::Solid)
            .ok_or_else(|| format!("SyntaxError: unsupported color '{c}'"));
    }
    if let Some(g) = v.get("grad") {
        let num = |k: &str| g.get(k).and_then(|x| x.as_f64()).unwrap_or(0.0) as f32;
        let mut stops: Vec<(f32, rendrix_css::Color)> = Vec::new();
        for s in g
            .get("stops")
            .and_then(|s| s.as_array())
            .cloned()
            .unwrap_or_default()
        {
            let off = s
                .get(0)
                .and_then(|x| x.as_f64())
                .unwrap_or(0.0)
                .clamp(0.0, 1.0) as f32;
            let col = s.get(1).and_then(|x| x.as_str()).unwrap_or("");
            let col = canvas::parse_color(col)
                .ok_or_else(|| format!("SyntaxError: unsupported gradient color '{col}'"))?;
            stops.push((off, col));
        }
        if stops.is_empty() {
            return Err("SyntaxError: gradient needs at least one color stop".to_string());
        }
        stops.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
        return Ok(Paint::Linear(LinearGradient {
            x0: num("x0"),
            y0: num("y0"),
            x1: num("x1"),
            y1: num("y1"),
            stops,
        }));
    }
    Err("SyntaxError: invalid paint style".to_string())
}

/// `drawImage` source resolution. `src_json` is `{"canvas":id}` (same document) or
/// `{"img":url}` (decoded via the engine image cache; missing/failed loads are `None`).
fn draw_image(host: &DomHost, n: u32, src_json: &str, args: &[Val]) -> Result<(), String> {
    let v: serde_json::Value = serde_json::from_str(src_json)
        .map_err(|_| "SyntaxError: invalid drawImage source".to_string())?;
    // (sx, sy, sw, sh, dx, dy, dw, dh) — overloads are expanded in JS, native takes all eight.
    let (sx, sy, sw, sh, dx, dy, dw, dh) = (
        num(args, 2),
        num(args, 3),
        num(args, 4),
        num(args, 5),
        num(args, 6),
        num(args, 7),
        num(args, 8),
        num(args, 9),
    );
    if sw == 0.0 || sh == 0.0 {
        return Ok(());
    }
    let key = doc_key(host);
    if let Some(src_id) = v.get("canvas").and_then(|x| x.as_f64()).map(|x| x as u32) {
        let bits = canvas::read_canvas(key, src_id, |c| c.map(|c| (c.pixels().to_vec(), c.size())));
        let Some((bits, (sw_full, sh_full))) = bits else {
            return Ok(());
        };
        let (sw_full, sh_full) = (sw_full as f32, sh_full as f32);
        if sx < 0.0 || sy < 0.0 || sx + sw > sw_full || sy + sh > sh_full {
            return Ok(()); // out-of-source-bounds draws paint nothing (documented subset behavior)
        }
        with(host, n, 0, 0, |c| {
            c.draw_image(
                &bits,
                sw_full as u32,
                sh_full as u32,
                sx,
                sy,
                sw,
                sh,
                dx,
                dy,
                dw,
                dh,
            )
        })??;
        return Ok(());
    }
    if let Some(url) = v.get("img").and_then(|x| x.as_str()) {
        let entry = host.engine.borrow().image_entry(url);
        let Some(entry) = entry else { return Ok(()) }; // not loaded yet: paints nothing
        if sx < 0.0 || sy < 0.0 || sx + sw > entry.width as f32 || sy + sh > entry.height as f32 {
            return Ok(());
        }
        with(host, n, 0, 0, |c| {
            c.draw_image(
                &entry.rgba,
                entry.width,
                entry.height,
                sx,
                sy,
                sw,
                sh,
                dx,
                dy,
                dw,
                dh,
            )
        })??;
        return Ok(());
    }
    Err("SyntaxError: invalid drawImage source".to_string())
}

/// One upload per canvas with undischarged pixels (clears dirty flags). The frame shell drains
/// this after `Engine::update` and feeds `Renderer::{upload_canvas, update_canvas_region}`.
pub struct CanvasUpload {
    pub node: u32,
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
    pub dirty: Option<[u32; 4]>,
}

pub fn take_canvas_uploads(engine: &rendrix_core::Engine) -> Vec<CanvasUpload> {
    let key = (&engine.doc as *const rendrix_dom::Document) as usize;
    // 3D scenes render into their canvas bitmaps first, so the drain below uploads 3D
    // content through the existing canvas texture path (windowed + headless screenshots).
    crate::scene3d::flush_scenes(engine);
    // Reconcile attribute sizes first (a `setAttribute('width',…)` without a JS setter call
    // still resizes + clears the bitmap, per spec). A size change marks the whole bitmap so
    // the recreated GPU target is refreshed even with no further draws.
    for (n, w, h) in doc_canvases(engine) {
        let changed = canvas::read_canvas(key, n, |c| {
            c.map(|c| c.size() != (w, h)).unwrap_or(w != 0 && h != 0)
        });
        canvas::with_canvas(key, n, w, h, |_| {});
        if changed {
            canvas::with_canvas(key, n, w, h, |c| c.mark_full());
        }
    }
    canvas::take_uploads(key)
        .into_iter()
        .map(|u| CanvasUpload {
            node: u.node,
            width: u.width,
            height: u.height,
            rgba: u.rgba,
            dirty: u.dirty,
        })
        .collect()
}

fn doc_canvases(engine: &rendrix_core::Engine) -> Vec<(u32, u32, u32)> {
    let root = engine.doc.root();
    engine
        .doc
        .depth_first(root)
        .into_iter()
        .filter(|n| engine.doc.tag(*n) == Some("canvas"))
        .map(|n| {
            let dim = |name: &str, def: f32| {
                engine
                    .doc
                    .get_attr(n, name)
                    .and_then(|s| s.trim().parse::<f64>().ok())
                    .unwrap_or(def as f64)
                    .max(0.0)
                    .min(canvas::MAX_CANVAS_DIM as f64) as u32
            };
            (n, dim("width", 300.0), dim("height", 150.0))
        })
        .collect()
}
