//! 3D scene store: the JS-owned side of the scene API.
//!
//! The GPU backend owns real scene rendering long-term (`rendrix-gfx` will grow a
//! `scene3d` module with `upsert_scene(id, desc_json)` / `remove_scene(id)`). Until that
//! lands, this module is a thin, self-contained shim with the **same JSON schema** so the
//! JS API never changes: scenes live here, keyed by hosting document, and are rasterized
//! on the CPU into their canvas bitmap. The frame shell already drains canvas uploads
//! (`take_canvas_uploads`), so 3D content flows through the existing canvas texture path
//! and shows up in windowed frames and headless `--screenshot` captures with no changes
//! to `rendrix-gfx`, `rendrix-core`, `window.rs` or `headless.rs`.
//!
//! ## Schema (shared with the future gfx backend)
//!
//! ```json
//! {
//!   "camera":   { "fovY": 45, "near": 0.1, "far": 100,
//!                 "position": [0,0,5], "lookAt": [0,0,0] },
//!   "meshes":   [ { "geo": { "kind": "box", "size": [2,2,2] },
//!                   "material": { "color": [1,0,0,1], "metal": 0, "rough": 0.8, "emissive": 0 },
//!                   "transform": { "pos": [0,0,0], "rotEulerDeg": [30,20,0], "scale": 1 } } ],
//!   "lights":   [ { "dir": [0.4,0.8,0.6] } ],
//!   "background": [0,0,0,0]
//! }
//! ```
//!
//! `geo.kind` is one of `"box"` (`size`: number or `[x,y,z]`, default 1), `"plane"`
//! (`size`: number or `[w,h]`, XY facing +Z, default 1), `"sphere"` (`size`: radius,
//! default 0.5, fixed 10x14 tessellation) or `"tris"` (`verts`: flat `[x,y,z,…]`,
//! multiple of 9; optional `indices` triples). `color` is `[r,g,b]` or `[r,g,b,a]` in
//! 0-1 (values >1 are treated as 0-255) or a CSS color string. Rotations are Euler
//! degrees applied Z, then Y, then X. `metal`/`rough` are parsed and ignored (deferred
//! PBR); lighting is one Lambert directional plus ambient.
//!
//! ## Rendering model (v1, documented limits)
//!
//! - A canvas with an active scene is **scene-owned**: every scene upload repaints the
//!   whole bitmap (transparent clear, then background, then meshes back-to-front with
//!   the painter's algorithm). 2D ops on the same canvas are overwritten by the next
//!   scene flush. No depth buffer, no backface culling, no clipping (a triangle with a
//!   vertex behind the near plane is skipped wholesale).
//! - Per-frame upload only when dirty: `upsert` marks the scene pending (plus the
//!   engine `paint` flag, reusing the Canvas 2D invalidation path — no restyle or
//!   relayout). `flush_scenes` renders pending scenes into their canvas bitmaps, which
//!   the existing `take_canvas_uploads` drain then uploads. Empty scenes with a
//!   transparent background are a no-op and never touch the bitmap.
//! - Element removal drops the scene: `sync_scenes` (called from `Session::frame`)
//!   prunes scenes whose nodes are dead, `take_freed` removes them eagerly, and
//!   `flush_scenes` prunes defensively. Stale pixels already composited stay until the
//!   next paint — removal stops future renders, it does not unpaint.

use crate::engine_trait::Val;
use crate::host::{base64_decode, DomHost};
use rendrix_gfx::canvas::{self, Paint};
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

type StoreKey = (usize, u32);

#[derive(Clone, Debug)]
struct StoredScene {
    desc: SceneFile,
    pending: bool,
    last_size: Option<(u32, u32)>,
}

fn store() -> &'static Mutex<HashMap<StoreKey, StoredScene>> {
    static S: OnceLock<Mutex<HashMap<StoreKey, StoredScene>>> = OnceLock::new();
    S.get_or_init(|| Mutex::new(HashMap::new()))
}

// ------------------------------------------------------------------------------------
// Schema
// ------------------------------------------------------------------------------------

#[derive(Clone, Debug)]
struct Camera {
    fov_y: f32,
    near: f32,
    far: f32,
    pos: [f32; 3],
    look_at: [f32; 3],
}

impl Default for Camera {
    fn default() -> Camera {
        Camera {
            fov_y: 45.0,
            near: 0.1,
            far: 100.0,
            pos: [0.0, 0.0, 5.0],
            look_at: [0.0, 0.0, 0.0],
        }
    }
}

#[derive(Clone, Debug)]
enum Geo {
    Box([f32; 3]),
    Plane([f32; 2]),
    Sphere(f32),
    Tris(Vec<[f32; 3]>, Option<Vec<[u32; 3]>>),
}

#[derive(Clone, Debug)]
struct Material {
    color: [f32; 4],
    emissive: f32,
    /// GPU face-cull mode: none | back | front.
    cull: String,
}

/// Light description shared by the CPU rasterizer and the GPU viewport. The CPU path
/// uses only the first directional light; the GPU path shades all of them.
#[derive(Clone, Debug)]
enum JLight {
    Directional { dir: [f32; 3], color: [f32; 3], intensity: f32 },
    Ambient { color: [f32; 3], intensity: f32 },
    Point { pos: [f32; 3], color: [f32; 3], intensity: f32, range: f32 },
}

#[derive(Clone, Debug)]
struct Transform {
    pos: [f32; 3],
    rot_deg: [f32; 3],
    scale: [f32; 3],
}

#[derive(Clone, Debug)]
struct Mesh {
    geo: Geo,
    mat: Material,
    xf: Transform,
}

#[derive(Clone, Debug)]
struct SceneFile {
    camera: Camera,
    meshes: Vec<Mesh>,
    light_dir: [f32; 3],
    lights: Vec<JLight>,
    background: [f32; 4],
}

fn num(v: &serde_json::Value) -> Option<f32> {
    v.as_f64().map(|n| n as f32).filter(|n| n.is_finite())
}

fn vec3(v: &serde_json::Value, def: [f32; 3]) -> [f32; 3] {
    match v.as_array() {
        Some(a) if a.len() >= 3 => [
            num(&a[0]).unwrap_or(def[0]),
            num(&a[1]).unwrap_or(def[1]),
            num(&a[2]).unwrap_or(def[2]),
        ],
        _ => def,
    }
}

/// `[r,g,b]` or `[r,g,b,a]` in 0-1 (any component >1 switches the whole tuple to 0-255)
/// or a CSS color string. Anything else falls back to `def`.
fn color_of(v: Option<&serde_json::Value>, def: [f32; 4]) -> [f32; 4] {
    match v {
        Some(serde_json::Value::Array(a)) if a.len() == 3 || a.len() == 4 => {
            let mut c = [
                num(&a[0]).unwrap_or(def[0]),
                num(&a[1]).unwrap_or(def[1]),
                num(&a[2]).unwrap_or(def[2]),
                a.get(3).and_then(num).unwrap_or(def[3]),
            ];
            if c.iter().any(|x| *x > 1.0) {
                for x in &mut c {
                    *x = (*x / 255.0).clamp(0.0, 1.0);
                }
            } else {
                for x in &mut c {
                    *x = x.clamp(0.0, 1.0);
                }
            }
            c
        }
        Some(serde_json::Value::String(s)) => canvas::parse_color(s)
            .map(|c| [c.r, c.g, c.b, c.a])
            .unwrap_or(def),
        _ => def,
    }
}

/// `size` for box/plane/sphere: number, `[s]` or per-axis array.
fn geo_size(v: Option<&serde_json::Value>) -> Vec<f32> {
    match v {
        Some(serde_json::Value::Number(_)) => vec![num(v.unwrap()).unwrap_or(1.0)],
        Some(serde_json::Value::Array(a)) => a.iter().filter_map(num).collect(),
        _ => vec![],
    }
}

fn parse_camera(v: Option<&serde_json::Value>) -> Camera {
    let mut c = Camera::default();
    let Some(o) = v.and_then(|v| v.as_object()) else {
        return c;
    };
    if let Some(f) = o.get("fovY").and_then(num) {
        if f > 1.0 && f < 179.0 {
            c.fov_y = f;
        }
    }
    if let Some(n) = o.get("near").and_then(num) {
        if n > 0.0 {
            c.near = n;
        }
    }
    if let Some(f) = o.get("far").and_then(num) {
        if f > c.near {
            c.far = f;
        }
    }
    c.pos = o
        .get("position")
        .map(|v| vec3(v, c.pos))
        .unwrap_or(c.pos);
    c.look_at = o
        .get("lookAt")
        .map(|v| vec3(v, c.look_at))
        .unwrap_or(c.look_at);
    c
}

fn parse_transform(v: Option<&serde_json::Value>) -> Transform {
    let mut t = Transform {
        pos: [0.0, 0.0, 0.0],
        rot_deg: [0.0, 0.0, 0.0],
        scale: [1.0, 1.0, 1.0],
    };
    let Some(o) = v.and_then(|v| v.as_object()) else {
        return t;
    };
    t.pos = o.get("pos").map(|v| vec3(v, t.pos)).unwrap_or(t.pos);
    t.rot_deg = o
        .get("rotEulerDeg")
        .or_else(|| o.get("rot"))
        .map(|v| vec3(v, t.rot_deg))
        .unwrap_or(t.rot_deg);
    t.scale = match o.get("scale") {
        Some(serde_json::Value::Number(_)) => {
            let s = num(o.get("scale").unwrap()).unwrap_or(1.0);
            if s == 0.0 {
                [1.0, 1.0, 1.0]
            } else {
                [s, s, s]
            }
        }
        Some(v) => {
            let s = vec3(v, t.scale);
            if s.iter().all(|x| *x == 0.0) {
                [1.0, 1.0, 1.0]
            } else {
                s
            }
        }
        None => t.scale,
    };
    t
}

fn parse_geo(v: &serde_json::Value) -> Result<Geo, String> {
    let o = v
        .as_object()
        .ok_or_else(|| "NotSupportedError: mesh.geo must be an object".to_string())?;
    let kind = o
        .get("kind")
        .and_then(|k| k.as_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    let size = geo_size(o.get("size"));
    let at = |i: usize, def: f32| size.get(i).copied().filter(|v| *v > 0.0).unwrap_or(def);
    match kind.as_str() {
        "box" | "cube" => Ok(Geo::Box([at(0, 1.0), at(1, at(0, 1.0)), at(2, at(0, 1.0))])),
        "plane" | "quad" => Ok(Geo::Plane([at(0, 1.0), at(1, at(0, 1.0))])),
        "sphere" | "ball" => Ok(Geo::Sphere(at(0, 0.5))),
        "tris" | "triangles" => {
            let flat: Vec<f32> = o
                .get("verts")
                .and_then(|v| v.as_array())
                .map(|a| a.iter().filter_map(num).collect())
                .unwrap_or_default();
            if flat.is_empty() || flat.len() % 9 != 0 {
                return Err("NotSupportedError: tris geo needs verts as a flat [x,y,z,…] array with a multiple of 9 numbers".to_string());
            }
            let pts: Vec<[f32; 3]> = flat
                .chunks_exact(3)
                .map(|c| [c[0], c[1], c[2]])
                .collect();
            let idx = match o.get("indices").and_then(|v| v.as_array()) {
                Some(a) => {
                    let flat_idx: Vec<u32> = a
                        .iter()
                        .filter_map(|v| v.as_u64().map(|n| n as u32))
                        .collect();
                    if flat_idx.len() % 3 != 0
                        || flat_idx.iter().any(|i| (*i as usize) >= pts.len())
                    {
                        return Err("NotSupportedError: tris indices must be triples into verts".to_string());
                    }
                    Some(
                        flat_idx
                            .chunks_exact(3)
                            .map(|c| [c[0], c[1], c[2]])
                            .collect(),
                    )
                }
                None => None,
            };
            Ok(Geo::Tris(pts, idx))
        }
        other => Err(format!(
            "NotSupportedError: unsupported geo kind '{other}' (box, plane, sphere, tris)"
        )),
    }
}

fn parse_scene(json: &str) -> Result<SceneFile, String> {
    let v: serde_json::Value =
        serde_json::from_str(json).map_err(|_| "SyntaxError: scene description is not valid JSON".to_string())?;
    let o = v
        .as_object()
        .ok_or_else(|| "TypeError: scene description must be an object".to_string())?;
    let camera = parse_camera(o.get("camera"));
    let meshes = o
        .get("meshes")
        .and_then(|m| m.as_array())
        .cloned()
        .unwrap_or_default();
    if meshes.len() > 256 {
        return Err("NotSupportedError: too many meshes (max 256)".to_string());
    }
    let mut out = Vec::with_capacity(meshes.len());
    for m in &meshes {
        let mo = m
            .as_object()
            .ok_or_else(|| "TypeError: mesh must be an object".to_string())?;
        let geo = parse_geo(mo.get("geo").unwrap_or(&serde_json::Value::Null))?;
        let mat = Material {
            color: color_of(
                mo.get("material").and_then(|m| m.get("color")),
                [1.0, 1.0, 1.0, 1.0],
            ),
            emissive: mo
                .get("material")
                .and_then(|m| m.get("emissive"))
                .and_then(num)
                .unwrap_or(0.0)
                .clamp(0.0, 1.0),
            cull: match mo.get("material").and_then(|m| m.get("cull")).and_then(|v| v.as_str()) {
                Some("back") => "back".to_string(),
                Some("front") => "front".to_string(),
                _ => "none".to_string(),
            },
        };
        out.push(Mesh {
            geo,
            mat,
            xf: parse_transform(mo.get("transform")),
        });
    }
    let mut dir = [0.4f32, 0.8, 0.6];
    let mut lights: Vec<JLight> = Vec::new();
    if let Some(arr) = o.get("lights").and_then(|l| l.as_array()) {
        for l in arr {
            let kind = l.get("kind").and_then(|v| v.as_str()).unwrap_or("dir");
            let c = color_of(l.get("color"), [1.0, 1.0, 1.0, 1.0]);
            let color = [c[0], c[1], c[2]];
            let intensity = l.get("intensity").and_then(num).unwrap_or(1.0).max(0.0);
            match kind {
                "ambient" => lights.push(JLight::Ambient { color, intensity }),
                "point" => {
                    let pos = l
                        .get("pos")
                        .or_else(|| l.get("position"))
                        .map(|v| vec3(v, [0.0, 0.0, 0.0]))
                        .unwrap_or([0.0, 0.0, 0.0]);
                    let range = l.get("range").and_then(num).unwrap_or(0.0).max(0.0);
                    lights.push(JLight::Point { pos, color, intensity, range });
                }
                _ => {
                    let d = l.get("dir").or_else(|| l.get("direction")).map(|v| vec3(v, dir)).unwrap_or(dir);
                    if d != [0.0, 0.0, 0.0] {
                        dir = d;
                    }
                    lights.push(JLight::Directional { dir: d, color, intensity });
                }
            }
        }
    }
    if lights.is_empty() {
        lights.push(JLight::Directional { dir, color: [1.0, 1.0, 1.0], intensity: 1.0 });
    }
    let len = (dir[0] * dir[0] + dir[1] * dir[1] + dir[2] * dir[2]).sqrt();
    if len > 0.0 {
        dir = [dir[0] / len, dir[1] / len, dir[2] / len];
    }
    Ok(SceneFile {
        camera,
        meshes: out,
        light_dir: dir,
        lights,
        background: color_of(o.get("background").or_else(|| o.get("clearColor")), [0.0, 0.0, 0.0, 0.0]),
    })
}

// ------------------------------------------------------------------------------------
// Store + native ops
// ------------------------------------------------------------------------------------

/// The hosting document's address (same derivation as `canvas_ops::doc_key`).
pub fn doc_key_of(engine: &rendrix_core::Engine) -> usize {
    (&engine.doc as *const rendrix_dom::Document) as usize
}

fn attr_dim(engine: &rendrix_core::Engine, n: u32, name: &str, def: f32) -> u32 {
    engine
        .doc
        .get_attr(n, name)
        .and_then(|s| s.trim().parse::<f64>().ok())
        .unwrap_or(def as f64)
        .max(0.0)
        .min(canvas::MAX_CANVAS_DIM as f64) as u32
}

/// Route a `scene3d_*` op. Returns `None` when `op` is not a scene op.
pub fn scene_op(host: &DomHost, op: &str, args: &[Val]) -> Option<Result<Val, String>> {
    if !op.starts_with("scene3d_") {
        return None;
    }
    Some(run(host, op, args))
}

fn run(host: &DomHost, op: &str, args: &[Val]) -> Result<Val, String> {
    match op {
        "scene3d_upsert" => {
            let n = node(args, 0)?;
            let json = str_of(args, 1);
            upsert(host, n, &json)?;
            Ok(Val::Undefined)
        }
        "scene3d_remove" => {
            let n = node(args, 0)?;
            remove(doc_key(host), n);
            Ok(Val::Undefined)
        }
        "scene3d_get" => {
            let n = node(args, 0)?;
            let key = doc_key(host);
            let s = store();
            let s = s.lock().unwrap();
            match s.get(&(key, n)) {
                Some(st) => Ok(Val::Str(canonical_json(&st.desc))),
                None => Ok(Val::Null),
            }
        }
        "scene3d_texture" => {
            let id = num_arg(args, 0)? as u32;
            let payload = base64_decode(&str_of(args, 1))
                .ok_or_else(|| "EncodingError: scene3d_texture payload is not valid base64".to_string())?;
            let w = num_arg(args, 2)? as u32;
            let h = num_arg(args, 3)? as u32;
            rendrix_gfx::scene3d::set_scene_texture(id, payload, w, h)?;
            // Invalidate cached images using this material so the GPU pass re-uploads.
            host.engine.borrow_mut().doc.dirty.paint = true;
            Ok(Val::Undefined)
        }
        "scene3d_texture_remove" => {
            let id = num_arg(args, 0)? as u32;
            rendrix_gfx::scene3d::remove_scene_texture(id);
            Ok(Val::Undefined)
        }
        _ => Err(format!("unknown native operation '{op}'")),
    }
}

fn node(args: &[Val], i: usize) -> Result<u32, String> {
    args.get(i)
        .and_then(|v| v.as_f64())
        .map(|n| n as u32)
        .ok_or_else(|| format!("TypeError: scene op argument {i} must be a node id"))
}

fn num_arg(args: &[Val], i: usize) -> Result<f64, String> {
    args.get(i)
        .and_then(|v| v.as_f64())
        .ok_or_else(|| format!("TypeError: scene op argument {i} must be a number"))
}

fn str_of(args: &[Val], i: usize) -> String {
    args.get(i)
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string()
}

fn doc_key(host: &DomHost) -> usize {
    doc_key_of(&host.engine.borrow())
}

fn upsert(host: &DomHost, n: u32, json: &str) -> Result<(), String> {
    {
        let e = host.engine.borrow();
        let tag = e.doc.tag(n);
        if !e.doc.is_alive(n) || !matches!(tag, Some("canvas") | Some("rendrix-scene")) {
            return Err("InvalidStateError: scene host element is not in the document".to_string());
        }
    }
    let desc = parse_scene(json)?;
    let key = doc_key(host);
    let size = {
        let e = host.engine.borrow();
        (attr_dim(&e, n, "width", 300.0), attr_dim(&e, n, "height", 150.0))
    };
    let mut s = store().lock().unwrap();
    let e = s.entry((key, n)).or_insert(StoredScene {
        desc: desc.clone(),
        pending: false,
        last_size: None,
    });
    // A size change always re-renders (the bitmap is recreated empty by the canvas path).
    if e.last_size != Some(size) {
        e.pending = true;
    }
    e.desc = desc.clone();
    e.pending = true;
    let canonical = canonical_json(&desc);
    drop(s);
    // Bridge to the GPU backend: the same desc drives `Item::Scene3D` viewports
    // (`<rendrix-scene>` / `<canvas data-3d>`) while the CPU rasterizer above covers
    // plain `<canvas>` bitmaps. Best-effort: a gfx-side parse rejection never fails
    // the JS call (the CPU path is the source of truth for validation).
    // Best-effort, but never silent: a schema mismatch here used to be discarded and left
    // `<rendrix-scene>` / `<canvas data-3d>` hosts with no GPU scene at all.
    if let Err(e) = rendrix_gfx::scene3d::upsert_scene(n as u64, &canonical) {
        eprintln!("[rendrix] GPU scene bridge rejected scene {n}: {e}");
    }
    // Invalidation reuses the Canvas 2D path: the element repaints without restyle/relayout.
    canvas::mark_dirty(key, n);
    host.engine.borrow_mut().doc.dirty.paint = true;
    Ok(())
}

fn remove(doc: usize, n: u32) {
    let mut s = store().lock().unwrap();
    s.remove(&(doc, n));
    drop(s);
    rendrix_gfx::scene3d::remove_scene(n as u64);
}

/// Drop scenes for a batch of freed node ids (called from `take_freed`).
pub fn remove_many(doc: usize, ids: &[u32]) {
    if ids.is_empty() {
        return;
    }
    let mut s = store().lock().unwrap();
    for n in ids {
        s.remove(&(doc, *n));
        rendrix_gfx::scene3d::remove_scene(*n as u64);
    }
}

/// Lifecycle sync, called from `Session::frame` after `Engine::update`: prune scenes of
/// dead nodes and re-arm scenes whose backing size changed (CSS layout rect changes only
/// affect display scaling, which the compositor handles — only the `width`/`height`
/// backing store matters here).
pub fn sync_scenes(engine: &mut rendrix_core::Engine) {
    let doc = doc_key_of(engine);
    {
        let s = store().lock().unwrap();
        if !s.keys().any(|(d, _)| *d == doc) {
            return; // no scenes for this document: zero per-frame cost
        }
    }
    let mut sizes: HashMap<u32, (u32, u32)> = HashMap::new();
    {
        let root = engine.doc.root();
        for n in engine.doc.depth_first(root) {
            if matches!(engine.doc.tag(n), Some("canvas") | Some("rendrix-scene")) {
                sizes.insert(
                    n,
                    (
                        attr_dim(engine, n, "width", 300.0),
                        attr_dim(engine, n, "height", 150.0),
                    ),
                );
            }
        }
    }
    let mut s = store().lock().unwrap();
    let mut dropped: Vec<u32> = Vec::new();
    s.retain(|(d, n), _| {
        let keep = *d != doc || engine.doc.is_alive(*n);
        if !keep && *d == doc {
            dropped.push(*n);
        }
        keep
    });
    drop(s);
    for n in dropped {
        rendrix_gfx::scene3d::remove_scene(n as u64);
    }
    let mut s = store().lock().unwrap();
    let mut repaint = false;
    for ((d, n), st) in s.iter_mut() {
        if *d != doc {
            continue;
        }
        match sizes.get(n) {
            Some(size) if Some(*size) != st.last_size => {
                st.pending = true;
                repaint = true;
            }
            None => {
                st.pending = false;
            }
            _ => {}
        }
    }
    // Scenes whose element is gone entirely (never in the map because the tag walk only
    // sees canvases) are dropped by the retain above via is_alive; nodes missing from the
    // walk but alive (e.g. detached) keep their stored scene without rendering.
    if repaint {
        engine.doc.dirty.paint = true;
    }
}

/// Drop scenes hosted by nodes that fail `keep`. The scene store is process-global
/// and keyed by document address (see `doc_key_of`), so a fresh document can reuse
/// a freed one's address: without pruning, a stale scene from a dead document could
/// repaint into the new one. Called from `take_canvas_uploads` before flushing.
pub fn prune_stale(doc: usize, keep: impl Fn(u32) -> bool) {
    store().lock().unwrap().retain(|(d, n), _| *d != doc || keep(*n));
}

/// Render every dirty scene into its canvas bitmap. Called at the top of
/// `take_canvas_uploads`, so both the window driver and headless screenshots pick up 3D
/// content through the existing canvas texture path.
pub fn flush_scenes(engine: &rendrix_core::Engine) {
    let doc = doc_key_of(engine);
    {
        let s = store().lock().unwrap();
        if !s.keys().any(|(d, _)| *d == doc) {
            return; // no scenes for this document: the canvas drain below is untouched
        }
    }
    // Collect first: rendering touches the canvas store while the scene store is locked.
    let mut work: Vec<(u32, u32, u32)> = Vec::new();
    {
        let mut s = store().lock().unwrap();
        // Defensive prune (the eager paths are `take_freed` + `sync_scenes`).
        s.retain(|(d, n), _| *d != doc || engine.doc.is_alive(*n));
        for ((d, n), st) in s.iter_mut() {
            if *d != doc {
                continue;
            }
            let size = (
                attr_dim(engine, *n, "width", 300.0),
                attr_dim(engine, *n, "height", 150.0),
            );
            if st.pending || st.last_size != Some(size) {
                st.pending = false;
                st.last_size = Some(size);
                // Empty scenes over transparency never touch the bitmap (so a bare
                // getContext('webgl2') feature probe cannot wipe 2D content).
                if st.desc.meshes.is_empty() && st.desc.background[3] <= 0.0 {
                    continue;
                }
                work.push((*n, size.0, size.1));
            }
        }
    }
    for (n, w, h) in work {
        if w == 0 || h == 0 {
            continue;
        }
        let desc = store()
            .lock()
            .unwrap()
            .get(&(doc, n))
            .map(|st| st.desc.clone());
        if let Some(desc) = desc {
            canvas::with_canvas(doc, n, w, h, |c| paint_scene(c, &desc, w, h));
        }
    }
}

/// Canonical JSON of a stored scene (for `scene3d_get` / `RendrixScene.getScene`).
fn canonical_json(d: &SceneFile) -> String {
    let meshes: Vec<serde_json::Value> = d
        .meshes
        .iter()
        .map(|m| {
            // The GPU backend uses unit primitives and applies `transform.scale`, while the
            // JS schema carries a per-geometry `size`. Bake size into the scale so both the
            // CPU bitmap rasterizer and the GPU viewport render the same sized object.
            let (kind, extra, size_scale) = match &m.geo {
                Geo::Box(s) => ("box", serde_json::json!({}), *s),
                Geo::Plane(s) => ("plane", serde_json::json!({}), [s[0], s[1], 1.0]),
                Geo::Sphere(r) => ("sphere", serde_json::json!({}), [*r * 2.0, *r * 2.0, *r * 2.0]),
                Geo::Tris(pts, idx) => (
                    "tris",
                    serde_json::json!({
                        "verts": pts.iter().flat_map(|p| [p[0], p[1], p[2]]).collect::<Vec<f32>>(),
                        "indices": idx,
                    }),
                    [1.0, 1.0, 1.0],
                ),
            };
            let mut geo = serde_json::Map::new();
            geo.insert("kind".into(), kind.into());
            for (k, v) in extra.as_object().unwrap() {
                geo.insert(k.clone(), v.clone());
            }
            let scale: Vec<f32> =
                m.xf.scale.iter().zip(size_scale).map(|(a, b)| a * b).collect();
            // `emissive` must be a 3-vector: the GPU MatDesc deserializes Vec<f32>, and a
            // JSON number here silently rejected every scene before this fix.
            let emissive = [m.mat.emissive, m.mat.emissive, m.mat.emissive];
            serde_json::json!({
                "geo": geo,
                "material": { "color": m.mat.color, "emissive": emissive, "cull": m.mat.cull },
                "transform": { "pos": m.xf.pos, "rotEulerDeg": m.xf.rot_deg, "scale": scale },
            })
        })
        .collect();
    let lights: Vec<serde_json::Value> = d
        .lights
        .iter()
        .map(|l| match l {
            JLight::Directional { dir, color, intensity } => serde_json::json!({
                "kind": "dir", "dir": dir, "color": color, "intensity": intensity,
            }),
            JLight::Ambient { color, intensity } => serde_json::json!({
                "kind": "ambient", "color": color, "intensity": intensity,
            }),
            JLight::Point { pos, color, intensity, range } => serde_json::json!({
                "kind": "point", "pos": pos, "color": color, "intensity": intensity, "range": range,
            }),
        })
        .collect();
    serde_json::json!({
        "camera": {
            "fovY": d.camera.fov_y, "near": d.camera.near, "far": d.camera.far,
            "position": d.camera.pos, "lookAt": d.camera.look_at,
        },
        "meshes": meshes,
        "lights": lights,
        "background": d.background,
    })
    .to_string()
}

// ------------------------------------------------------------------------------------
// CPU rasterizer
// ------------------------------------------------------------------------------------

fn paint_scene(
    c: &mut rendrix_gfx::canvas::Canvas2D,
    desc: &SceneFile,
    w: u32,
    h: u32,
) {
    use rendrix_css::Color;
    c.save();
    c.set_transform([1.0, 0.0, 0.0, 1.0, 0.0, 0.0]);
    c.set_alpha(1.0);
    // Scene-owned repaint: start from a clean bitmap every upload (predictable frames).
    c.clear_rect(0.0, 0.0, w as f32, h as f32);
    let bg = desc.background;
    if bg[3] > 0.0 {
        c.set_fill(Paint::Solid(Color::rgba(bg[0], bg[1], bg[2], bg[3])));
        c.fill_rect(0.0, 0.0, w as f32, h as f32);
    }
    if w == 0 || h == 0 || desc.meshes.is_empty() {
        c.restore();
        return;
    }
    let aspect = w as f32 / h as f32;
    let cam = camera_basis(&desc.camera);
    // Collect world-space shaded triangles, then paint far-to-near.
    let mut tris: Vec<ShadedTri> = Vec::new();
    for m in &desc.meshes {
        for t in local_tris(&m.geo) {
            let a = model_point(&m.xf, t[0]);
            let b = model_point(&m.xf, t[1]);
            let c3 = model_point(&m.xf, t[2]);
            let n = model_normal(&m.xf, face_normal(t[0], t[1], t[2]));
            let shade = shade_color(&m.mat, n, desc.light_dir);
            tris.push(ShadedTri {
                w: [a, b, c3],
                color: shade,
            });
        }
    }
    let mut draw: Vec<(f32, [[f32; 2]; 3], [f32; 4])> = Vec::with_capacity(tris.len());
    for t in &tris {
        let mut depth = 0.0;
        let mut pts = [[0.0f32; 2]; 3];
        let mut ok = true;
        for (i, p) in t.w.iter().enumerate() {
            let v = to_view(&cam, *p);
            if v[2] < desc.camera.near.max(0.01) || v[2] > desc.camera.far {
                ok = false;
                break;
            }
            depth += v[2] / 3.0;
            let s = project_point(&desc.camera, &cam, v, w, h, aspect);
            if !s[0].is_finite() || !s[1].is_finite() {
                ok = false;
                break;
            }
            pts[i] = s;
        }
        if !ok {
            continue;
        }
        // Skip degenerates.
        let area = (pts[1][0] - pts[0][0]) * (pts[2][1] - pts[0][1])
            - (pts[2][0] - pts[0][0]) * (pts[1][1] - pts[0][1]);
        if area.abs() < 0.5 {
            continue;
        }
        draw.push((depth, pts, t.color));
    }
    draw.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
    for (_, pts, col) in draw {
        c.set_fill(Paint::Solid(Color::rgba(col[0], col[1], col[2], col[3])));
        c.begin_path();
        c.move_to(pts[0][0], pts[0][1]);
        c.line_to(pts[1][0], pts[1][1]);
        c.line_to(pts[2][0], pts[2][1]);
        c.close_path();
        c.fill();
    }
    c.restore();
}

struct ShadedTri {
    w: [[f32; 3]; 3],
    color: [f32; 4],
}

struct CamBasis {
    pos: [f32; 3],
    right: [f32; 3],
    up: [f32; 3],
    fwd: [f32; 3],
    tan_half: f32,
}

fn norm3(v: [f32; 3]) -> [f32; 3] {
    let l = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
    if l < 1e-9 {
        [0.0, 0.0, 0.0]
    } else {
        [v[0] / l, v[1] / l, v[2] / l]
    }
}

fn cross(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

fn dot(a: [f32; 3], b: [f32; 3]) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

fn camera_basis(cam: &Camera) -> CamBasis {
    let fwd = norm3([
        cam.look_at[0] - cam.pos[0],
        cam.look_at[1] - cam.pos[1],
        cam.look_at[2] - cam.pos[2],
    ]);
    let fwd = if fwd == [0.0, 0.0, 0.0] {
        [0.0, 0.0, -1.0]
    } else {
        fwd
    };
    let up_hint = if fwd[1].abs() > 0.999 {
        [0.0, 0.0, 1.0]
    } else {
        [0.0, 1.0, 0.0]
    };
    let right = norm3(cross(fwd, up_hint));
    let up = cross(right, fwd);
    CamBasis {
        pos: cam.pos,
        right,
        up,
        fwd,
        tan_half: (cam.fov_y.to_radians() / 2.0).tan().max(1e-4),
    }
}

fn to_view(cam: &CamBasis, p: [f32; 3]) -> [f32; 3] {
    let d = [p[0] - cam.pos[0], p[1] - cam.pos[1], p[2] - cam.pos[2]];
    [dot(d, cam.right), dot(d, cam.up), dot(d, cam.fwd)]
}

fn project_point(
    cam: &Camera,
    basis: &CamBasis,
    v: [f32; 3],
    w: u32,
    h: u32,
    aspect: f32,
) -> [f32; 2] {
    let _ = cam;
    let nx = v[0] / (v[2] * basis.tan_half * aspect);
    let ny = v[1] / (v[2] * basis.tan_half);
    [
        (nx * 0.5 + 0.5) * w as f32,
        (1.0 - (ny * 0.5 + 0.5)) * h as f32,
    ]
}

fn rot_matrix_xyz(deg: [f32; 3]) -> [[f32; 3]; 3] {
    // R = Rx * Ry * Rz (extrinsic X, then Y, then Z): applied to column vectors, Z first.
    let (x, y, z) = (
        deg[0].to_radians(),
        deg[1].to_radians(),
        deg[2].to_radians(),
    );
    let (cx, sx) = (x.cos(), x.sin());
    let (cy, sy) = (y.cos(), y.sin());
    let (cz, sz) = (z.cos(), z.sin());
    let rx = [[1.0, 0.0, 0.0], [0.0, cx, -sx], [0.0, sx, cx]];
    let ry = [[cy, 0.0, sy], [0.0, 1.0, 0.0], [-sy, 0.0, cy]];
    let rz = [[cz, -sz, 0.0], [sz, cz, 0.0], [0.0, 0.0, 1.0]];
    mat_mul(mat_mul(rx, ry), rz)
}

fn mat_mul(a: [[f32; 3]; 3], b: [[f32; 3]; 3]) -> [[f32; 3]; 3] {
    let mut o = [[0.0; 3]; 3];
    for r in 0..3 {
        for c in 0..3 {
            o[r][c] = a[r][0] * b[0][c] + a[r][1] * b[1][c] + a[r][2] * b[2][c];
        }
    }
    o
}

fn mat_vec(m: [[f32; 3]; 3], v: [f32; 3]) -> [f32; 3] {
    [
        m[0][0] * v[0] + m[0][1] * v[1] + m[0][2] * v[2],
        m[1][0] * v[0] + m[1][1] * v[1] + m[1][2] * v[2],
        m[2][0] * v[0] + m[2][1] * v[1] + m[2][2] * v[2],
    ]
}

fn model_point(t: &Transform, p: [f32; 3]) -> [f32; 3] {
    let s = [p[0] * t.scale[0], p[1] * t.scale[1], p[2] * t.scale[2]];
    let r = mat_vec(rot_matrix_xyz(t.rot_deg), s);
    [r[0] + t.pos[0], r[1] + t.pos[1], r[2] + t.pos[2]]
}

fn model_normal(t: &Transform, n: [f32; 3]) -> [f32; 3] {
    // Rotation only (non-uniform scale skew is ignored in v1 — documented).
    norm3(mat_vec(rot_matrix_xyz(t.rot_deg), n))
}

fn face_normal(a: [f32; 3], b: [f32; 3], c: [f32; 3]) -> [f32; 3] {
    norm3(cross(
        [b[0] - a[0], b[1] - a[1], b[2] - a[2]],
        [c[0] - a[0], c[1] - a[1], c[2] - a[2]],
    ))
}

fn shade_color(mat: &Material, n: [f32; 3], light: [f32; 3]) -> [f32; 4] {
    let diff = dot(n, light).max(0.0);
    let k = (0.35 + 0.65 * diff + mat.emissive).min(1.5);
    [
        (mat.color[0] * k).clamp(0.0, 1.0),
        (mat.color[1] * k).clamp(0.0, 1.0),
        (mat.color[2] * k).clamp(0.0, 1.0),
        mat.color[3].clamp(0.0, 1.0),
    ]
}

fn local_tris(geo: &Geo) -> Vec<[[f32; 3]; 3]> {
    match geo {
        Geo::Box([sx, sy, sz]) => {
            let (x, y, z) = (sx / 2.0, sy / 2.0, sz / 2.0);
            let v = [
                [-x, -y, -z],
                [x, -y, -z],
                [x, y, -z],
                [-x, y, -z],
                [-x, -y, z],
                [x, -y, z],
                [x, y, z],
                [-x, y, z],
            ];
            // Outward-wound quads: front(+z), back(−z), right(+x), left(−x), top(+y), bottom(−y).
            let quads = [
                [4, 5, 6, 7],
                [1, 0, 3, 2],
                [5, 1, 2, 6],
                [0, 4, 7, 3],
                [7, 6, 2, 3],
                [0, 1, 5, 4],
            ];
            quads
                .iter()
                .flat_map(|q| [ [v[q[0]], v[q[1]], v[q[2]]], [v[q[0]], v[q[2]], v[q[3]]] ])
                .collect()
        }
        Geo::Plane([w, h]) => {
            let (x, y) = (w / 2.0, h / 2.0);
            vec![
                [[-x, -y, 0.0], [x, -y, 0.0], [x, y, 0.0]],
                [[-x, -y, 0.0], [x, y, 0.0], [-x, y, 0.0]],
            ]
        }
        Geo::Sphere(r) => {
            let (lat, lon) = (10usize, 14usize);
            let pt = |i: usize, j: usize| {
                let theta = std::f32::consts::PI * i as f32 / lat as f32;
                let phi = 2.0 * std::f32::consts::PI * j as f32 / lon as f32;
                [
                    r * theta.sin() * phi.cos(),
                    r * theta.cos(),
                    r * theta.sin() * phi.sin(),
                ]
            };
            let mut out = Vec::new();
            for i in 0..lat {
                for j in 0..lon {
                    let j1 = (j + 1) % lon;
                    out.push([pt(i, j), pt(i + 1, j), pt(i + 1, j1)]);
                    out.push([pt(i, j), pt(i + 1, j1), pt(i, j1)]);
                }
            }
            out
        }
        Geo::Tris(pts, idx) => match idx {
            Some(idx) => idx
                .iter()
                .map(|t| [pts[t[0] as usize], pts[t[1] as usize], pts[t[2] as usize]])
                .collect(),
            None => pts.chunks_exact(3).map(|c| [c[0], c[1], c[2]]).collect(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scene(json: &str) -> SceneFile {
        parse_scene(json).expect("valid scene")
    }

    #[test]
    fn box_scene_parses_with_defaults() {
        let d = scene(
            r#"{"camera": {"fovY": 45, "position": [0,0,6], "lookAt": [0,0,0]},
                 "meshes": [{"geo": {"kind": "box", "size": [2,2,2]},
                             "material": {"color": [255,0,0,255]},
                             "transform": {"rotEulerDeg": [30,30,0]}}]}"#,
        );
        assert_eq!(d.meshes.len(), 1);
        assert_eq!(d.meshes[0].mat.color, [1.0, 0.0, 0.0, 1.0]);
        assert_eq!(d.camera.pos, [0.0, 0.0, 6.0]);
        assert_eq!(d.camera.near, 0.1, "default near survives");
    }

    #[test]
    fn unknown_geo_kind_is_a_notsupported_error() {
        let e = parse_scene(r#"{"meshes": [{"geo": {"kind": "torus"}}]}"#).unwrap_err();
        assert!(e.starts_with("NotSupportedError"), "{e}");
    }

    #[test]
    fn bad_tris_verts_are_rejected() {
        let e = parse_scene(r#"{"meshes": [{"geo": {"kind": "tris", "verts": [0,1]}}]}"#).unwrap_err();
        assert!(e.starts_with("NotSupportedError"), "{e}");
        // Indexed form works.
        let d = scene(
            r#"{"meshes": [{"geo": {"kind": "tris",
                 "verts": [0,0,0, 1,0,0, 0,1,0], "indices": [0,1,2]}}]}"#,
        );
        assert_eq!(local_tris(&d.meshes[0].geo).len(), 1);
    }

    #[test]
    fn css_colors_and_255_tuples_work() {
        let d = scene(r#"{"meshes": [{"geo": {"kind": "plane"}, "material": {"color": "red"}}]}"#);
        assert_eq!(d.meshes[0].mat.color, [1.0, 0.0, 0.0, 1.0]);
        let d = scene(r#"{"meshes": [{"geo": {"kind": "plane"}, "material": {"color": [0,128,0]}}]}"#);
        let g = d.meshes[0].mat.color[1];
        assert!((g - 128.0 / 255.0).abs() < 1e-6, "{g}");
    }

    #[test]
    fn box_projects_to_screen_center() {
        let d = scene(
            r#"{"camera": {"position": [0,0,6], "lookAt": [0,0,0]},
                 "meshes": [{"geo": {"kind": "box", "size": [2,2,2]}}]}"#,
        );
        let cam = camera_basis(&d.camera);
        // The box center (origin) is straight ahead: NDC (0,0), screen center of 200x100.
        let v = to_view(&cam, [0.0, 0.0, 0.0]);
        assert!((v[2] - 6.0).abs() < 1e-5, "{v:?}");
        let s = project_point(&d.camera, &cam, v, 200, 100, 2.0);
        assert!((s[0] - 100.0).abs() < 1e-3 && (s[1] - 50.0).abs() < 1e-3, "{s:?}");
    }
}
