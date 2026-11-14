//! CPU-side 3D scene store + the JSON contract used by script.
//!
//! A scene is keyed by a `u64` scene id (the DOM `NodeId` of the `<canvas data-3d>`
//! or `<rendrix-scene>` element that hosts it). Script (via the JS-side agent) calls
//! [`upsert_scene`] / [`remove_scene`]; the renderer reads scenes back when it composites
//! [`SceneDraw`] viewports from the display list.
//!
//! ## JSON contract (v1)
//!
//! ```json
//! {
//!   "camera": { "fovY": 60.0, "near": 0.1, "far": 100.0,
//!               "position": [0, 0, 5], "lookAt": [0, 0, 0] },
//!   "meshes": [ {
//!       "geo": { "kind": "box" },
//!       "material": { "color": [1, 0, 0, 1], "metal": 0.0, "rough": 0.8,
//!                     "emissive": [0, 0, 0] },
//!       "transform": { "pos": [0, 0, 0], "rotEulerDeg": [0, 30, 0], "scale": [1, 1, 1] }
//!   } ],
//!   "lights": [ { "kind": "dir", "dir": [0.4, -1, 0.6],
//!                 "color": [1, 1, 1], "intensity": 1.0 },
//!               { "kind": "ambient", "color": [1, 1, 1], "intensity": 0.35 } ]
//! }
//! ```
//!
//! * `camera.*` all have defaults (`fovY` 60, `near` 0.1, `far` 100,
//!   `position` [0,0,5], `lookAt` [0,0,0]); world up is fixed at +Y.
//! * `geo.kind` is one of `"box"` | `"plane"` | `"sphere"` | `"tris"`.
//!   Procedural geometry only in v1: unit box (side 1, centred on the origin),
//!   a 1x1 ground plane in XZ facing +Y, and a low-poly UV sphere (radius 0.5).
//!   `"tris"` takes raw triangle soup: `"verts"` (flat `[x,y,z, ...]`, required),
//!   optional `"normals"` (same layout; smooth normals are derived when omitted),
//!   optional `"indices"` (sequential when omitted).
//! * `material` defaults: white, `metal` 0, `rough` 0.8, no emissive.
//!   `color` accepts `[r,g,b]` or `[r,g,b,a]` in 0..1. `texture_id` is parsed and
//!   stored but **ignored by the v1 renderer** (see deferred work below).
//! * `transform` defaults to identity. `scale` accepts a scalar or `[x,y,z]`.
//!   Model matrix order: `M = T(pos) * Ry(yaw) * Rx(pitch) * Rz(roll) * S(scale)`.
//! * `lights` defaults to one directional + one ambient light when omitted or empty.
//!   Only the **first** directional light and the **sum** of ambient lights drive the
//!   v1 shader; extras are parsed, stored, and ignored.
//!
//! ## Extension points (deferred: shadows, glTF, PBR textures)
//!
//! * glTF loading: parse into [`Geometry::Tris`] (positions + indices; derive normals
//!   with [`derive_smooth_normals`]) and [`Scene::from_json`]-style materials. No
//!   loader ships in v1 on purpose: the whole pipeline below the JSON boundary already
//!   speaks indexed triangles.
//! * PBR textures: [`Material::texture_id`] is reserved for a GPU image handle;
//!   the shader's `params.w` channel is free for a "has texture" flag.
//! * Shadows: the forward pass has no shadow map; add a depth-only pass + sampler.

use glam::{EulerRot, Mat4, Vec3, Vec4};
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

/// Scene id = the hosting element's DOM `NodeId`.
pub type SceneId = u64;

/// Right-handed, Y-up world. Camera looks down -Z in view space (glam/wgpu convention).
#[derive(Debug, Clone)]
pub struct Camera {
    pub fov_y_deg: f32,
    pub near: f32,
    pub far: f32,
    pub position: Vec3,
    pub target: Vec3,
}

impl Default for Camera {
    fn default() -> Self {
        Camera { fov_y_deg: 60.0, near: 0.1, far: 100.0, position: Vec3::new(0.0, 0.0, 5.0), target: Vec3::ZERO }
    }
}

impl Camera {
    /// `proj * view` for a viewport with `width/height` aspect. Uses the wgpu NDC
    /// convention (Z in [0,1], Y-up); aspect is guarded against degenerate rects.
    pub fn view_proj(&self, width: f32, height: f32) -> Mat4 {
        let aspect = if height.abs() < 1e-6 { 1.0 } else { (width / height).max(1e-6) };
        let proj = glam::camera::rh::proj::directx::perspective(self.fov_y_deg.to_radians(), aspect, self.near, self.far);
        let view = glam::camera::rh::view::look_at_mat4(self.position, self.target, Vec3::Y);
        proj * view
    }
}

/// One vertex of CPU-side geometry: position + normal.
#[derive(Debug, Clone, Copy, Default)]
pub struct Vertex3 {
    pub pos: [f32; 3],
    pub normal: [f32; 3],
}

/// Geometry kinds. Only procedural shapes + raw triangles in v1 (no glTF loader).
#[derive(Debug, Clone)]
pub enum Geometry {
    Box,
    Plane,
    Sphere,
    Tris { verts: Vec<Vertex3>, indices: Vec<u32> },
}

impl Geometry {
    /// Local-space bounding sphere (centre, radius) for frustum culling.
    pub fn bounds(&self) -> ([f32; 3], f32) {
        match self {
            Geometry::Box => ([0.0; 3], 0.8660255),
            Geometry::Plane => ([0.0; 3], 0.7071068),
            Geometry::Sphere => ([0.0; 3], 0.5),
            Geometry::Tris { verts, .. } => bound_verts(verts),
        }
    }
}

fn bound_verts(verts: &[Vertex3]) -> ([f32; 3], f32) {
    if verts.is_empty() {
        return ([0.0; 3], 0.0);
    }
    let mut min = verts[0].pos;
    let mut max = verts[0].pos;
    for v in &verts[1..] {
        for i in 0..3 {
            min[i] = min[i].min(v.pos[i]);
            max[i] = max[i].max(v.pos[i]);
        }
    }
    let c = [(min[0] + max[0]) / 2.0, (min[1] + max[1]) / 2.0, (min[2] + max[2]) / 2.0];
    let r = verts.iter().map(|v| dist3(v.pos, c)).fold(0.0f32, f32::max);
    (c, r)
}

fn dist3(a: [f32; 3], b: [f32; 3]) -> f32 {
    ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2) + (a[2] - b[2]).powi(2)).sqrt()
}

/// Surface response. `texture_id` is stored for a future textured-PBR pass and
/// ignored by the v1 forward renderer.
#[derive(Debug, Clone)]
pub struct Material {
    pub base_color: [f32; 4],
    pub metallic: f32,
    pub roughness: f32,
    pub emissive: [f32; 3],
    pub texture_id: Option<u32>,
}

impl Default for Material {
    fn default() -> Self {
        Material { base_color: [1.0, 1.0, 1.0, 1.0], metallic: 0.0, roughness: 0.8, emissive: [0.0; 3], texture_id: None }
    }
}

#[derive(Debug, Clone)]
pub struct Mesh {
    pub geometry: Geometry,
    pub material: Material,
    /// World matrix; build with [`mat4_from_trs`].
    pub transform: Mat4,
    /// Local-space bounding sphere, derived from the geometry at parse time.
    pub bounds: ([f32; 3], f32),
}

impl Mesh {
    /// World-space bounding sphere: centre transformed, radius grown by the largest
    /// basis-vector length (exact for uniform scale, conservative otherwise).
    pub fn world_sphere(&self) -> ([f32; 3], f32) {
        let c = self.transform.transform_point3(Vec3::from(self.bounds.0));
        let sx = self.transform.x_axis.truncate().length();
        let sy = self.transform.y_axis.truncate().length();
        let sz = self.transform.z_axis.truncate().length();
        ([c.x, c.y, c.z], self.bounds.1 * sx.max(sy).max(sz).max(1e-9))
    }
}

/// v1 lighting: one directional + ambient. Extra lights parse but do not shade.
#[derive(Debug, Clone)]
pub enum Light {
    /// `dir` is the direction light travels (like the CSS/sample convention here).
    Directional { dir: Vec3, color: [f32; 3], intensity: f32 },
    Ambient { color: [f32; 3], intensity: f32 },
}

#[derive(Debug, Clone)]
pub struct Scene {
    pub camera: Camera,
    pub meshes: Vec<Mesh>,
    pub lights: Vec<Light>,
    /// Bumped on every upsert; the renderer keys its GPU mesh cache on it.
    pub revision: u64,
}

impl Scene {
    /// First directional light, else a fixed frustration-free default.
    pub fn key_light(&self) -> (Vec3, [f32; 3]) {
        for l in &self.lights {
            if let Light::Directional { dir, color, intensity } = l {
                let d = if dir.length_squared() > 1e-12 { dir.normalize() } else { Vec3::new(0.35, -0.7, -0.9).normalize() };
                return (d, [color[0] * intensity, color[1] * intensity, color[2] * intensity]);
            }
        }
        (Vec3::new(0.35, -0.7, -0.9).normalize(), [1.0, 1.0, 1.0])
    }

    /// Sum of ambient lights, else a fixed default so unlit scenes stay readable.
    pub fn ambient(&self) -> [f32; 3] {
        let mut acc = [0.0f32; 3];
        let mut any = false;
        for l in &self.lights {
            if let Light::Ambient { color, intensity } = l {
                any = true;
                for i in 0..3 {
                    acc[i] += color[i] * intensity;
                }
            }
        }
        if any { acc } else { [0.35, 0.35, 0.35] }
    }

    /// Indices of meshes whose world sphere intersects the view frustum.
    pub fn visible_meshes(&self, view_proj: &Mat4) -> Vec<usize> {
        let planes = frustum_planes(view_proj);
        self.meshes
            .iter()
            .enumerate()
            .filter(|(_, m)| {
                let (c, r) = m.world_sphere();
                sphere_visible(&planes, Vec3::from(c), r)
            })
            .map(|(i, _)| i)
            .collect()
    }
}

/// `M = T(pos) * Ry(yaw) * Rx(pitch) * Rz(roll) * S(scale)`, angles in degrees.
pub fn mat4_from_trs(pos: [f32; 3], euler_deg: [f32; 3], scale: [f32; 3]) -> Mat4 {
    let t = Mat4::from_translation(Vec3::from(pos));
    let r = Mat4::from_euler(
        EulerRot::YXZ,
        euler_deg[1].to_radians(),
        euler_deg[0].to_radians(),
        euler_deg[2].to_radians(),
    );
    let s = Mat4::from_scale(Vec3::from(scale));
    t * r * s
}

/// Six inward-facing frustum planes (normal + distance) for NDC x/y in [-1,1], z in [0,1].
pub fn frustum_planes(vp: &Mat4) -> [Vec4; 6] {
    // Rows of the column-major matrix.
    let row = |i: usize| Vec4::new(vp.x_axis[i], vp.y_axis[i], vp.z_axis[i], vp.w_axis[i]);
    let (r0, r1, r2, r3) = (row(0), row(1), row(2), row(3));
    let norm = |mut p: Vec4| {
        let l = p.truncate().length().max(1e-9);
        p /= l;
        p
    };
    [
        norm(r3 + r0), // left
        norm(r3 - r0), // right
        norm(r3 + r1), // bottom
        norm(r3 - r1), // top
        norm(r2),      // near (z >= 0)
        norm(r3 - r2), // far  (z <= w)
    ]
}

/// True when the sphere is (at least partly) inside every frustum plane.
pub fn sphere_visible(planes: &[Vec4; 6], center: Vec3, radius: f32) -> bool {
    planes.iter().all(|p| p.truncate().dot(center) + p.w >= -radius)
}

// -------------------------------------------------------------------------------------------
// Procedural geometry
// -------------------------------------------------------------------------------------------

/// Unit box, side 1 centred on the origin, with per-face normals (24 verts).
pub fn build_box() -> (Vec<Vertex3>, Vec<u32>) {
    // (face normal, 4 corners CCW from outside)
    let faces: [([f32; 3], [[f32; 3]; 4]); 6] = [
        ([1.0, 0.0, 0.0], [[0.5, -0.5, -0.5], [0.5, -0.5, 0.5], [0.5, 0.5, 0.5], [0.5, 0.5, -0.5]]),
        ([-1.0, 0.0, 0.0], [[-0.5, -0.5, 0.5], [-0.5, -0.5, -0.5], [-0.5, 0.5, -0.5], [-0.5, 0.5, 0.5]]),
        ([0.0, 1.0, 0.0], [[-0.5, 0.5, -0.5], [0.5, 0.5, -0.5], [0.5, 0.5, 0.5], [-0.5, 0.5, 0.5]]),
        ([0.0, -1.0, 0.0], [[-0.5, -0.5, 0.5], [0.5, -0.5, 0.5], [0.5, -0.5, -0.5], [-0.5, -0.5, -0.5]]),
        ([0.0, 0.0, 1.0], [[-0.5, -0.5, 0.5], [-0.5, 0.5, 0.5], [0.5, 0.5, 0.5], [0.5, -0.5, 0.5]]),
        ([0.0, 0.0, -1.0], [[0.5, -0.5, -0.5], [0.5, 0.5, -0.5], [-0.5, 0.5, -0.5], [-0.5, -0.5, -0.5]]),
    ];
    let mut verts = Vec::with_capacity(24);
    let mut idx = Vec::with_capacity(36);
    for (n, corners) in faces {
        let base = verts.len() as u32;
        for p in corners {
            verts.push(Vertex3 { pos: p, normal: n });
        }
        // Corner lists run CCW seen from outside; keep that winding in the triangles.
        idx.extend_from_slice(&[base, base + 2, base + 1, base, base + 3, base + 2]);
    }
    (verts, idx)
}

/// 1x1 ground plane in XZ facing +Y.
pub fn build_plane() -> (Vec<Vertex3>, Vec<u32>) {
    let verts = vec![
        Vertex3 { pos: [-0.5, 0.0, -0.5], normal: [0.0, 1.0, 0.0] },
        Vertex3 { pos: [0.5, 0.0, -0.5], normal: [0.0, 1.0, 0.0] },
        Vertex3 { pos: [0.5, 0.0, 0.5], normal: [0.0, 1.0, 0.0] },
        Vertex3 { pos: [-0.5, 0.0, 0.5], normal: [0.0, 1.0, 0.0] },
    ];
    (verts, vec![0, 2, 1, 0, 3, 2])
}

/// Low-poly UV sphere, radius 0.5 (12 segments x 8 rings), with single-vertex
/// poles fanned (no degenerate pole-ring triangles).
pub fn build_sphere() -> (Vec<Vertex3>, Vec<u32>) {
    let (segs, rings) = (12usize, 8usize);
    let mut verts: Vec<Vertex3> = Vec::with_capacity(2 + (rings - 1) * segs);
    verts.push(Vertex3 { pos: [0.0, 0.5, 0.0], normal: [0.0, 1.0, 0.0] });
    for j in 1..rings {
        let theta = std::f32::consts::PI * j as f32 / rings as f32;
        let (st, ct) = (theta.sin(), theta.cos());
        for i in 0..segs {
            let phi = 2.0 * std::f32::consts::PI * i as f32 / segs as f32;
            let (sp, cp) = (phi.sin(), phi.cos());
            let n = [st * cp, ct, st * sp];
            verts.push(Vertex3 { pos: [n[0] * 0.5, n[1] * 0.5, n[2] * 0.5], normal: n });
        }
    }
    let bottom = verts.len() as u32;
    verts.push(Vertex3 { pos: [0.0, -0.5, 0.0], normal: [0.0, -1.0, 0.0] });
    let mut idx: Vec<u32> = Vec::with_capacity((rings - 1) * segs * 6);
    // Top fan (outward = pole, next-phi, phi).
    for i in 0..segs {
        idx.extend_from_slice(&[0, (1 + (i + 1) % segs) as u32, (1 + i) as u32]);
    }
    // Middle bands.
    for j in 1..rings - 1 {
        let (r0, r1) = (1 + (j - 1) * segs, 1 + j * segs);
        for i in 0..segs {
            let i1 = (i + 1) % segs;
            idx.extend_from_slice(&[(r0 + i) as u32, (r0 + i1) as u32, (r1 + i) as u32, (r0 + i1) as u32, (r1 + i1) as u32, (r1 + i) as u32]);
        }
    }
    // Bottom fan (outward = pole, phi, next-phi — the mirror of the top fan).
    let rb = 1 + (rings - 2) * segs;
    for i in 0..segs {
        idx.extend_from_slice(&[bottom, (rb + i) as u32, (rb + (i + 1) % segs) as u32]);
    }
    (verts, idx)
}

/// Area-weighted smooth normals, matching vertex count of `positions`.
pub fn derive_smooth_normals(positions: &[[f32; 3]], indices: &[u32]) -> Vec<[f32; 3]> {
    let mut acc = vec![[0.0f32; 3]; positions.len()];
    for tri in indices.chunks_exact(3) {
        let (a, b, c) = (positions[tri[0] as usize], positions[tri[1] as usize], positions[tri[2] as usize]);
        let (u, v) = ([b[0] - a[0], b[1] - a[1], b[2] - a[2]], [c[0] - a[0], c[1] - a[1], c[2] - a[2]]);
        let n = [u[1] * v[2] - u[2] * v[1], u[2] * v[0] - u[0] * v[2], u[0] * v[1] - u[1] * v[0]];
        for k in tri {
            for i in 0..3 {
                acc[*k as usize][i] += n[i];
            }
        }
    }
    acc.into_iter()
        .map(|n| {
            let l = (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt();
            if l > 1e-12 { [n[0] / l, n[1] / l, n[2] / l] } else { [0.0, 1.0, 0.0] }
        })
        .collect()
}

// -------------------------------------------------------------------------------------------
// JSON parsing with validation
// -------------------------------------------------------------------------------------------

use serde::Deserialize;

#[derive(Debug, Deserialize, Default)]
struct Desc {
    #[serde(default)]
    camera: CameraDesc,
    #[serde(default)]
    meshes: Vec<MeshDesc>,
    #[serde(default)]
    lights: Vec<LightDesc>,
}

#[derive(Debug, Deserialize)]
struct CameraDesc {
    #[serde(rename = "fovY", default = "d_fov")]
    fov_y: f32,
    #[serde(default = "d_near")]
    near: f32,
    #[serde(default = "d_far")]
    far: f32,
    #[serde(default = "d_cam_pos")]
    position: [f32; 3],
    #[serde(rename = "lookAt", default = "d_target")]
    look_at: [f32; 3],
}

impl Default for CameraDesc {
    fn default() -> Self {
        CameraDesc { fov_y: d_fov(), near: d_near(), far: d_far(), position: d_cam_pos(), look_at: d_target() }
    }
}

fn d_fov() -> f32 {
    60.0
}
fn d_near() -> f32 {
    0.1
}
fn d_far() -> f32 {
    100.0
}
fn d_cam_pos() -> [f32; 3] {
    [0.0, 0.0, 5.0]
}
fn d_target() -> [f32; 3] {
    [0.0, 0.0, 0.0]
}

#[derive(Debug, Deserialize, Default)]
struct MeshDesc {
    #[serde(default)]
    geo: GeoDesc,
    #[serde(default)]
    material: MatDesc,
    #[serde(default)]
    transform: TrDesc,
}

#[derive(Debug, Deserialize)]
struct GeoDesc {
    #[serde(default = "d_kind")]
    kind: String,
    #[serde(default)]
    verts: Option<Vec<serde_json::Value>>,
    #[serde(default)]
    normals: Option<Vec<f32>>,
    #[serde(default)]
    indices: Option<Vec<u32>>,
}

impl Default for GeoDesc {
    fn default() -> Self {
        GeoDesc { kind: d_kind(), verts: None, normals: None, indices: None }
    }
}

fn d_kind() -> String {
    "box".to_string()
}

#[derive(Debug, Deserialize)]
struct MatDesc {
    #[serde(default = "d_color")]
    color: Vec<f32>,
    #[serde(rename = "metal", default)]
    metallic: f32,
    #[serde(rename = "rough", default = "d_rough")]
    roughness: f32,
    #[serde(default = "d_emis")]
    emissive: Vec<f32>,
    #[serde(rename = "textureId", default)]
    texture_id: Option<u32>,
}

impl Default for MatDesc {
    fn default() -> Self {
        MatDesc { color: d_color(), metallic: 0.0, roughness: d_rough(), emissive: d_emis(), texture_id: None }
    }
}

fn d_color() -> Vec<f32> {
    vec![1.0, 1.0, 1.0, 1.0]
}
fn d_rough() -> f32 {
    0.8
}
fn d_emis() -> Vec<f32> {
    vec![0.0, 0.0, 0.0]
}

#[derive(Debug, Deserialize)]
struct TrDesc {
    #[serde(default = "d_pos")]
    pos: Vec<f32>,
    #[serde(rename = "rotEulerDeg", default = "d_rot")]
    rot: Vec<f32>,
    #[serde(default = "d_scl")]
    scale: Vec<f32>,
}

impl Default for TrDesc {
    fn default() -> Self {
        TrDesc { pos: d_pos(), rot: d_rot(), scale: d_scl() }
    }
}

fn d_pos() -> Vec<f32> {
    vec![0.0, 0.0, 0.0]
}
fn d_rot() -> Vec<f32> {
    vec![0.0, 0.0, 0.0]
}
fn d_scl() -> Vec<f32> {
    vec![1.0, 1.0, 1.0]
}

#[derive(Debug, Deserialize)]
struct LightDesc {
    #[serde(default = "d_light_kind")]
    kind: String,
    #[serde(default = "d_dir")]
    dir: Vec<f32>,
    #[serde(default = "d_light_color")]
    color: Vec<f32>,
    #[serde(default = "d_intensity")]
    intensity: f32,
}

fn d_light_kind() -> String {
    "dir".to_string()
}
fn d_dir() -> Vec<f32> {
    vec![0.35, -0.7, -0.9]
}
fn d_light_color() -> Vec<f32> {
    vec![1.0, 1.0, 1.0]
}
fn d_intensity() -> f32 {
    1.0
}

fn f3(name: &str, v: &[f32], what: &str) -> Result<[f32; 3], String> {
    if v.len() != 3 {
        return Err(format!("{name}: {what} must have exactly 3 numbers, got {}", v.len()));
    }
    if v.iter().any(|x| !x.is_finite()) {
        return Err(format!("{name}: {what} must be finite numbers"));
    }
    Ok([v[0], v[1], v[2]])
}

fn color(name: &str, v: &[f32]) -> Result<[f32; 4], String> {
    if v.len() != 3 && v.len() != 4 {
        return Err(format!("{name}: color must have 3 or 4 numbers, got {}", v.len()));
    }
    if v.iter().any(|x| !x.is_finite()) {
        return Err(format!("{name}: color must be finite numbers"));
    }
    let cl = |x: f32| x.clamp(0.0, 1.0);
    Ok([cl(v[0]), cl(v[1]), cl(v[2]), if v.len() == 4 { cl(v[3]) } else { 1.0 }])
}

fn emissive(name: &str, v: &[f32]) -> Result<[f32; 3], String> {
    if v.len() != 3 {
        return Err(format!("{name}: emissive must have exactly 3 numbers, got {}", v.len()));
    }
    if v.iter().any(|x| !x.is_finite()) {
        return Err(format!("{name}: emissive must be finite numbers"));
    }
    Ok([v[0].max(0.0), v[1].max(0.0), v[2].max(0.0)])
}

fn scale_vec(v: &[f32]) -> Result<[f32; 3], String> {
    match v.len() {
        1 => Ok([v[0], v[0], v[0]]),
        3 => Ok([v[0], v[1], v[2]]),
        n => Err(format!("transform.scale must have 1 or 3 numbers, got {n}")),
    }
}

impl Scene {
    /// Parse + validate the JSON contract. `revision` is assigned by the store.
    pub fn from_json(json: &str) -> Result<Scene, String> {
        let desc: Desc = serde_json::from_str(json).map_err(|e| format!("scene: invalid JSON: {e}"))?;
        let c = desc.camera;
        if !(c.fov_y > 0.0 && c.fov_y < 180.0) {
            return Err(format!("scene.camera.fovY must be in (0, 180), got {}", c.fov_y));
        }
        if !(c.near > 0.0) {
            return Err(format!("scene.camera.near must be > 0, got {}", c.near));
        }
        if !(c.far > c.near) {
            return Err(format!("scene.camera.far ({}) must exceed near ({})", c.far, c.near));
        }
        for (k, v) in [("position", &c.position), ("lookAt", &c.look_at)] {
            if v.iter().any(|x| !x.is_finite()) {
                return Err(format!("scene.camera.{k} must be finite numbers"));
            }
        }
        let camera =
            Camera { fov_y_deg: c.fov_y, near: c.near, far: c.far, position: Vec3::from(c.position), target: Vec3::from(c.look_at) };

        let mut meshes = Vec::with_capacity(desc.meshes.len());
        for (i, m) in desc.meshes.iter().enumerate() {
            let tag = format!("scene.meshes[{i}]");
            let geometry = match m.geo.kind.as_str() {
                "box" => Geometry::Box,
                "plane" => Geometry::Plane,
                "sphere" => Geometry::Sphere,
                "tris" => {
                    let raw = m.geo.verts.as_ref().ok_or_else(|| format!("{tag}.geo: kind \"tris\" needs \"verts\""))?;
                    // Accept flat [x,y,z, ...] or nested [[x,y,z], ...].
                    let mut positions: Vec<[f32; 3]> = Vec::new();
                    if raw.first().is_some_and(|v| v.is_array()) {
                        for (j, v) in raw.iter().enumerate() {
                            let a = v.as_array().unwrap();
                            if a.len() != 3 || a.iter().any(|x| !x.as_f64().is_some_and(f64::is_finite)) {
                                return Err(format!("{tag}.geo.verts[{j}]: must be [x, y, z] finite numbers"));
                            }
                            positions.push([a[0].as_f64().unwrap() as f32, a[1].as_f64().unwrap() as f32, a[2].as_f64().unwrap() as f32]);
                        }
                    } else {
                        let flat: Vec<f32> = raw
                            .iter()
                            .map(|v| v.as_f64().filter(|x| x.is_finite()).unwrap_or(f64::NAN) as f32)
                            .collect();
                        if flat.iter().any(|x| !x.is_finite()) {
                            return Err(format!("{tag}.geo.verts: must be finite numbers"));
                        }
                        if flat.len() % 3 != 0 {
                            return Err(format!("{tag}.geo.verts: length must be a multiple of 3, got {}", flat.len()));
                        }
                        positions = flat.chunks_exact(3).map(|c| [c[0], c[1], c[2]]).collect();
                    }
                    if positions.len() < 3 {
                        return Err(format!("{tag}.geo.verts: need at least 3 vertices, got {}", positions.len()));
                    }
                    if positions.len() > 1_000_000 {
                        return Err(format!("{tag}.geo.verts: over the 1M-vertex v1 cap"));
                    }
                    let indices: Vec<u32> = match &m.geo.indices {
                        Some(ix) => {
                            for (j, k) in ix.iter().enumerate() {
                                if *k as usize >= positions.len() {
                                    return Err(format!("{tag}.geo.indices[{j}]: index {k} out of range ({})", positions.len()));
                                }
                            }
                            if ix.len() % 3 != 0 {
                                return Err(format!("{tag}.geo.indices: length must be a multiple of 3, got {}", ix.len()));
                            }
                            ix.clone()
                        }
                        None => {
                            if positions.len() % 3 != 0 {
                                return Err(format!("{tag}.geo: without indices, verts must form whole triangles (multiple of 3 vertices)"));
                            }
                            (0..positions.len() as u32).collect()
                        }
                    };
                    let normals: Vec<[f32; 3]> = match &m.geo.normals {
                        Some(n) => {
                            if n.len() != positions.len() * 3 {
                                return Err(format!(
                                    "{tag}.geo.normals: need {} numbers (3 per vertex), got {}",
                                    positions.len() * 3,
                                    n.len()
                                ));
                            }
                            if n.iter().any(|x| !x.is_finite()) {
                                return Err(format!("{tag}.geo.normals: must be finite numbers"));
                            }
                            n.chunks_exact(3)
                                .map(|c| {
                                    let l = (c[0] * c[0] + c[1] * c[1] + c[2] * c[2]).sqrt();
                                    if l > 1e-12 { [c[0] / l, c[1] / l, c[2] / l] } else { [0.0, 1.0, 0.0] }
                                })
                                .collect()
                        }
                        None => derive_smooth_normals(&positions, &indices),
                    };
                    let verts =
                        positions.into_iter().zip(normals).map(|(pos, normal)| Vertex3 { pos, normal }).collect();
                    Geometry::Tris { verts, indices }
                }
                other => return Err(format!("{tag}.geo.kind must be one of box|plane|sphere|tris, got \"{other}\"")),
            };
            let mat = &m.material;
            let base_color = color(&format!("{tag}.material"), &mat.color)?;
            if !mat.metallic.is_finite() || !(0.0..=1.0).contains(&mat.metallic) {
                return Err(format!("{tag}.material.metal must be in [0, 1], got {}", mat.metallic));
            }
            if !mat.roughness.is_finite() || !(0.0..=1.0).contains(&mat.roughness) {
                return Err(format!("{tag}.material.rough must be in [0, 1], got {}", mat.roughness));
            }
            let material = Material {
                base_color,
                metallic: mat.metallic,
                roughness: mat.roughness,
                emissive: emissive(&format!("{tag}.material"), &mat.emissive)?,
                texture_id: mat.texture_id,
            };
            let pos = f3(&tag, &m.transform.pos, "transform.pos")?;
            let rot = f3(&tag, &m.transform.rot, "transform.rotEulerDeg")?;
            if m.transform.scale.iter().any(|x| !x.is_finite()) {
                return Err(format!("{tag}: transform.scale must be finite numbers"));
            }
            let scl = scale_vec(&m.transform.scale).map_err(|e| format!("{tag}: {e}"))?;
            let transform = mat4_from_trs(pos, rot, scl);
            let bounds = geometry.bounds();
            meshes.push(Mesh { geometry, material, transform, bounds });
        }

        let mut lights = Vec::with_capacity(desc.lights.len());
        for (i, l) in desc.lights.iter().enumerate() {
            let tag = format!("scene.lights[{i}]");
            match l.kind.as_str() {
                "dir" => {
                    let dir = f3(&tag, &l.dir, "dir")?;
                    let color = emissive(&tag, &l.color)?;
                    if !l.intensity.is_finite() || l.intensity < 0.0 {
                        return Err(format!("{tag}: intensity must be >= 0, got {}", l.intensity));
                    }
                    lights.push(Light::Directional { dir: Vec3::from(dir), color, intensity: l.intensity });
                }
                "ambient" => {
                    let color = emissive(&tag, &l.color)?;
                    if !l.intensity.is_finite() || l.intensity < 0.0 {
                        return Err(format!("{tag}: intensity must be >= 0, got {}", l.intensity));
                    }
                    lights.push(Light::Ambient { color, intensity: l.intensity });
                }
                other => return Err(format!("{tag}: kind must be one of dir|ambient, got \"{other}\"")),
            }
        }

        Ok(Scene { camera, meshes, lights, revision: 0 })
    }
}

// -------------------------------------------------------------------------------------------
// Store: interior map + the process-global contract used by script
// -------------------------------------------------------------------------------------------

/// CPU-side scene map owned by whoever drives frames (unit-testable without globals).
#[derive(Debug, Default)]
pub struct SceneStore {
    scenes: HashMap<SceneId, Scene>,
    next_revision: u64,
}

impl SceneStore {
    pub fn new() -> Self {
        SceneStore::default()
    }

    /// Parse, validate and insert/replace the scene under `id`. Re-upserts bump the
    /// revision so the renderer rebuilds cached GPU buffers.
    pub fn upsert_json(&mut self, id: SceneId, desc_json: &str) -> Result<(), String> {
        let mut scene = Scene::from_json(desc_json)?;
        self.next_revision += 1;
        scene.revision = self.next_revision;
        self.scenes.insert(id, scene);
        Ok(())
    }

    pub fn remove(&mut self, id: SceneId) -> bool {
        self.scenes.remove(&id).is_some()
    }

    pub fn get(&self, id: SceneId) -> Option<&Scene> {
        self.scenes.get(&id)
    }

    pub fn ids(&self) -> Vec<SceneId> {
        self.scenes.keys().copied().collect()
    }

    pub fn len(&self) -> usize {
        self.scenes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.scenes.is_empty()
    }
}

static GLOBAL: OnceLock<Mutex<SceneStore>> = OnceLock::new();

fn global() -> &'static Mutex<SceneStore> {
    GLOBAL.get_or_init(|| Mutex::new(SceneStore::new()))
}

/// JS-agent contract: create or replace the scene hosted at `id`.
///
/// `desc_json` follows the schema documented at the top of this module; on error the
/// store is left untouched and the returned string names the offending field.
pub fn upsert_scene(id: SceneId, desc_json: &str) -> Result<(), String> {
    global().lock().map_err(|_| "scene store is poisoned".to_string())?.upsert_json(id, desc_json)
}

/// JS-agent contract: drop the scene hosted at `id`. Returns whether one existed.
pub fn remove_scene(id: SceneId) -> bool {
    global().lock().map(|mut s| s.remove(id)).unwrap_or(false)
}

/// JS-agent contract: ids of all live scenes.
pub fn scene_ids() -> Vec<SceneId> {
    global().lock().map(|s| s.ids()).unwrap_or_default()
}

/// Renderer hook: clone the scene for compositing (the lock is not held across GPU work).
pub(crate) fn cloned_scene(id: SceneId) -> Option<Scene> {
    global().lock().ok()?.get(id).cloned()
}

// -------------------------------------------------------------------------------------------
// Display-list viewport handed to the renderer
// -------------------------------------------------------------------------------------------

/// One scene's on-screen viewport in **physical pixels** (already scaled + clipped).
#[derive(Debug, Clone, Copy)]
pub struct SceneDraw {
    pub scene_id: SceneId,
    /// x, y, w, h in physical pixels, top-left origin.
    pub rect_px: [f32; 4],
}

#[cfg(test)]
mod tests {
    use super::*;

    const BOX: &str = r#"{
        "camera": { "fovY": 60, "near": 0.1, "far": 100, "position": [0, 1, 5], "lookAt": [0, 0, 0] },
        "meshes": [ { "geo": { "kind": "box" },
                      "material": { "color": [1, 0, 0, 1], "metal": 0.1, "rough": 0.5, "emissive": [0, 0, 0] },
                      "transform": { "pos": [0, 0, 0], "rotEulerDeg": [0, 30, 0], "scale": [2, 1, 1] } } ],
        "lights": [ { "kind": "dir", "dir": [0.4, -1, 0.6], "color": [1, 1, 1], "intensity": 1.2 },
                    { "kind": "ambient", "color": [0.5, 0.5, 0.6], "intensity": 0.5 } ]
    }"#;

    #[test]
    fn parses_the_full_contract() {
        let s = Scene::from_json(BOX).unwrap();
        assert_eq!(s.camera.fov_y_deg, 60.0);
        assert_eq!(s.meshes.len(), 1);
        assert!(matches!(s.meshes[0].geometry, Geometry::Box));
        assert_eq!(s.meshes[0].material.base_color, [1.0, 0.0, 0.0, 1.0]);
        assert_eq!(s.lights.len(), 2);
        let (dir, col) = s.key_light();
        assert!((dir.length() - 1.0).abs() < 1e-6, "key light dir is normalized");
        assert_eq!(col, [1.2, 1.2, 1.2]);
        assert_eq!(s.ambient(), [0.25, 0.25, 0.3]);
    }

    #[test]
    fn defaults_and_invalid_values() {
        let s = Scene::from_json("{}").unwrap();
        assert!(s.meshes.is_empty());
        assert!(s.lights.is_empty(), "empty lights: renderer falls back to defaults");
        assert_eq!(s.key_light().1, [1.0, 1.0, 1.0]);
        assert_eq!(s.ambient(), [0.35, 0.35, 0.35]);
        for (json, field) in [
            (r#"{"camera":{"fovY":0}}"#, "fovY"),
            (r#"{"camera":{"fovY":180}}"#, "fovY"),
            (r#"{"camera":{"near":-1}}"#, "near"),
            (r#"{"camera":{"near":5,"far":5}}"#, "far"),
            (r#"{"meshes":[{"geo":{"kind":"teapot"}}]}"#, "geo.kind"),
            (r#"{"meshes":[{"geo":{"kind":"tris"}}]}"#, "verts"),
            (r#"{"meshes":[{"geo":{"kind":"tris","verts":[0,1]}}]}"#, "verts"),
            (r#"{"meshes":[{"material":{"color":[1,2]}}]}"#, "color"),
            (r#"{"meshes":[{"material":{"metal":2}}]}"#, "metal"),
            (r#"{"meshes":[{"transform":{"scale":[1,2]}}]}"#, "scale"),
            (r#"{"lights":[{"kind":"point"}]}"#, "kind"),
            (r#"{"meshes":[{"geo":{"kind":"tris","verts":[[0,0,0],[1,0,0]]}}]}"#, "verts"),
        ] {
            let err = Scene::from_json(json).unwrap_err();
            assert!(err.contains(field), "{json} -> {err} (want {field})");
        }
        assert!(Scene::from_json("{oops").is_err(), "malformed JSON errors");
    }

    #[test]
    fn tris_parses_and_derives_normals() {
        let s = Scene::from_json(
            r#"{"meshes":[{"geo":{"kind":"tris",
                "verts":[[0,0,0],[1,0,0],[0,1,0],[0,0,1]],
                "indices":[0,1,2, 0,2,3]}}]}"#,
        )
        .unwrap();
        let Geometry::Tris { verts, indices } = &s.meshes[0].geometry else { panic!("want tris") };
        assert_eq!((verts.len(), indices.len()), (4, 6));
        for v in verts {
            let l = (v.normal[0].powi(2) + v.normal[1].powi(2) + v.normal[2].powi(2)).sqrt();
            assert!((l - 1.0).abs() < 1e-5, "normals are unit: {v:?}");
        }
        // Flat verts + explicit normals round-trip too.
        let s = Scene::from_json(
            r#"{"meshes":[{"geo":{"kind":"tris",
                "verts":[0,0,0, 1,0,0, 0,1,0],
                "normals":[0,0,1, 0,0,1, 0,0,1]}}]}"#,
        )
        .unwrap();
        let Geometry::Tris { verts, indices } = &s.meshes[0].geometry else { panic!("want tris") };
        assert_eq!((verts.len(), indices.len()), (3, 3));
        assert_eq!(verts[0].normal, [0.0, 0.0, 1.0]);
    }

    #[test]
    fn transform_matrix_order_is_t_rz_ry_rx_s() {
        // 90° yaw turns +X into -Z; scale applies first, translation last.
        let m = mat4_from_trs([10.0, 0.0, 0.0], [0.0, 90.0, 0.0], [2.0, 1.0, 1.0]);
        let p = m.transform_point3(Vec3::new(1.0, 0.0, 0.0));
        assert!((p - Vec3::new(10.0, 0.0, -2.0)).length() < 1e-4, "got {p:?}");
        // Identity is exact.
        assert_eq!(mat4_from_trs([0.0; 3], [0.0; 3], [1.0; 3]), Mat4::IDENTITY);
    }

    #[test]
    fn camera_projects_near_to_zero_and_far_to_one() {
        let c = Camera::default();
        let vp = c.view_proj(1.0, 1.0);
        let fwd = (c.target - c.position).normalize();
        for (dist, want) in [(c.near, 0.0), (c.far, 1.0)] {
            let p = c.position + fwd * dist;
            let clip = vp * glam::Vec4::new(p.x, p.y, p.z, 1.0);
            let z = clip.z / clip.w;
            assert!((z - want).abs() < 0.02, "depth {dist} -> NDC z {z} (want {want})");
        }
        // Perspective depth is nonlinear: the midpoint lands near the far plane.
        let mid = c.position + fwd * ((c.near + c.far) / 2.0);
        let clip = vp * glam::Vec4::new(mid.x, mid.y, mid.z, 1.0);
        let z = clip.z / clip.w;
        assert!((0.9..1.0).contains(&z), "mid depth -> NDC z {z}");
    }

    #[test]
    fn frustum_culling_keeps_inside_and_drops_outside() {
        let mut store = SceneStore::new();
        store
            .upsert_json(
                7,
                r#"{"meshes":[
                    {"geo":{"kind":"box"},"transform":{"pos":[0,0,0]}},
                    {"geo":{"kind":"box"},"transform":{"pos":[0,0,50]}},
                    {"geo":{"kind":"box"},"transform":{"pos":[40,0,0]}}]}"#,
            )
            .unwrap();
        let s = store.get(7).unwrap();
        let vp = s.camera.view_proj(800.0, 600.0);
        let vis = s.visible_meshes(&vp);
        assert_eq!(vis, vec![0], "origin box visible; far-behind and far-right culled: {vis:?}");
        // A huge sphere around the camera is always visible.
        assert!(sphere_visible(&frustum_planes(&vp), s.camera.position, 1000.0));
        assert!(!sphere_visible(&frustum_planes(&vp), Vec3::new(0.0, 0.0, 50.0), 0.5));
    }

    #[test]
    fn procedural_geometry_is_indexed_and_outward() {
        for (name, (verts, idx), outward) in [
            ("box", build_box(), None),
            ("plane", build_plane(), Some(Vec3::Y)),
            ("sphere", build_sphere(), None),
        ] {
            assert!(!verts.is_empty() && idx.len() % 3 == 0, "{name}: indexed triangles");
            let mut checked = 0;
            for tri in idx.chunks_exact(3) {
                let (a, b, c) = (verts[tri[0] as usize].pos, verts[tri[1] as usize].pos, verts[tri[2] as usize].pos);
                let (u, v) = (Vec3::from([b[0] - a[0], b[1] - a[1], b[2] - a[2]]), Vec3::from([c[0] - a[0], c[1] - a[1], c[2] - a[2]]));
                let n = u.cross(v);
                if n.length() < 1e-12 {
                    continue; // degenerate pole triangle: zero area, harmless
                }
                // Closed shapes: outward = away from the centroid; the plane carries
                // its own +Y reference (its centroid is perpendicular to the normal).
                let expect = outward.unwrap_or_else(|| {
                    Vec3::new((a[0] + b[0] + c[0]) / 3.0, (a[1] + b[1] + c[1]) / 3.0, (a[2] + b[2] + c[2]) / 3.0).normalize()
                });
                assert!(n.normalize().dot(expect) > 0.0, "{name}: triangle {tri:?} winds inward");
                checked += 1;
            }
            assert!(checked > 0, "{name}: no testable triangles");
            assert!(verts.iter().all(|v| {
                let l = (v.normal[0].powi(2) + v.normal[1].powi(2) + v.normal[2].powi(2)).sqrt();
                (l - 1.0).abs() < 1e-6
            }));
        }
        let (verts, _) = build_box();
        assert_eq!(verts.len(), 24);
        let (sv, si) = build_sphere();
        assert_eq!((sv.len(), si.len()), (2 + 7 * 12, 12 * 3 + 6 * 12 * 6 + 12 * 3), "12x8 UV sphere with fanned poles");
    }

    #[test]
    fn store_contract_round_trips() {
        let mut store = SceneStore::new();
        assert!(store.is_empty());
        store.upsert_json(1, BOX).unwrap();
        store.upsert_json(2, "{}").unwrap();
        assert_eq!(store.len(), 2);
        let r1 = store.get(1).unwrap().revision;
        store.upsert_json(1, "{}").unwrap();
        assert!(store.get(1).unwrap().revision > r1, "re-upsert bumps the revision");
        assert!(store.get(1).unwrap().meshes.is_empty(), "replace, not merge");
        assert!(store.remove(2));
        assert!(!store.remove(2));
        assert!(store.upsert_json(3, r#"{"camera":{"fovY":0}}"#).is_err(), "failed upsert leaves nothing");
        assert!(store.get(3).is_none());
    }

    #[test]
    fn world_sphere_grows_with_scale() {
        let m = Mesh {
            geometry: Geometry::Box,
            material: Material::default(),
            transform: mat4_from_trs([5.0, 0.0, 0.0], [0.0; 3], [2.0, 1.0, 1.0]),
            bounds: Geometry::Box.bounds(),
        };
        let (c, r) = m.world_sphere();
        assert_eq!(c, [5.0, 0.0, 0.0]);
        assert!((r - 0.8660255 * 2.0).abs() < 1e-5, "{r}");
    }
}
