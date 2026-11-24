//! v1 forward 3D pass: Blinn-Phong with up to 4 directional + 4 point lights and ambient.
//!
//! Two uniform buffers carry per-scene frame blocks (view-projection, camera,
//! lights) and per-mesh blocks (model + normal matrix, material). Each draw selects
//! its own aligned slot through dynamic uniform offsets, so one bind group and one
//! encoder serve every scene/mesh in the frame. Light arrays have fixed capacity so
//! the slot stride stays constant. Deferred: shadow maps, PBR texture sampling,
//! transparent sorting (the alpha channel is output but the opaque pipeline ignores it).

const MAX_DIR: u32 = 4u;
const MAX_POINT: u32 = 4u;

struct Frame {
    view_proj: mat4x4<f32>,
    cam_pos: vec4<f32>,
    // xyz = travel direction, w = intensity.
    dir: array<vec4<f32>, 4>,
    dir_color: array<vec4<f32>, 4>,
    // xyz = position, w = range (<= 0 = infinite).
    point: array<vec4<f32>, 4>,
    point_color: array<vec4<f32>, 4>,
    // x = directional count, y = point count.
    counts: vec4<f32>,
    ambient: vec4<f32>,
};

struct MeshU {
    model: mat4x4<f32>,
    // Inverse-transpose of model (CPU); keeps normals correct under non-uniform scale.
    normal_mat: mat4x4<f32>,
    base: vec4<f32>,
    emissive: vec4<f32>,
    // metal, roughness, shininess, flags.
    params: vec4<f32>,
};

@group(0) @binding(0) var<uniform> frame: Frame;
@group(0) @binding(1) var<uniform> mesh: MeshU;
@group(1) @binding(0) var tex: texture_2d<f32>;
@group(1) @binding(1) var samp: sampler;

struct VSOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) world: vec3<f32>,
    @location(1) normal: vec3<f32>,
    @location(2) uv: vec2<f32>,
};

@vertex
fn vs_main(@location(0) pos: vec3<f32>, @location(1) nrm: vec3<f32>, @location(2) uv: vec2<f32>) -> VSOut {
    let world = mesh.model * vec4<f32>(pos, 1.0);
    var out: VSOut;
    out.world = world.xyz;
    out.normal = normalize((mesh.normal_mat * vec4<f32>(nrm, 0.0)).xyz);
    out.uv = uv;
    out.clip = frame.view_proj * world;
    return out;
}

fn specular(n: vec3<f32>, l: vec3<f32>, v: vec3<f32>, shin: f32, rough: f32) -> f32 {
    let h = normalize(l + v);
    return pow(max(dot(n, h), 0.0), shin) * (1.0 - rough * 0.9);
}

@fragment
fn fs_main(in: VSOut) -> @location(0) vec4<f32> {
    let n = normalize(in.normal);
    let v = normalize(frame.cam_pos.xyz - in.world);
    let metal = mesh.params.x;
    let rough = mesh.params.y;
    let shin = mesh.params.z;
    // F0: dielectric 0.04, metals tint toward the base color (cheap F0 lerp, v1).
    let f0 = mix(vec3<f32>(0.04), mesh.base.rgb, metal);
    let albedo = mesh.base.rgb * (1.0 - metal * 0.85);
    var lit = frame.ambient.rgb;

    // Directional lights: travel direction is -l, as in the original single-light pass.
    for (var i = 0u; i < MAX_DIR; i = i + 1u) {
        if (f32(i) >= frame.counts.x) { break; }
        let dl = frame.dir[i];
        let l = normalize(-dl.xyz);
        let lc = frame.dir_color[i].rgb * dl.w;
        lit = lit + lc * (albedo * max(dot(n, l), 0.0) + f0 * specular(n, l, v, shin, rough));
    }

    // Point lights with optional linear-range falloff.
    for (var i = 0u; i < MAX_POINT; i = i + 1u) {
        if (f32(i) >= frame.counts.y) { break; }
        let pl = frame.point[i];
        let delta = pl.xyz - in.world;
        let dist = length(delta);
        let l = delta / max(dist, 1e-4);
        var atten = 1.0;
        if (pl.w > 0.0) {
            atten = pow(clamp(1.0 - dist / pl.w, 0.0, 1.0), 2.0);
        }
        let pc = frame.point_color[i];
        let lc = pc.rgb * pc.a * atten;
        lit = lit + lc * (albedo * max(dot(n, l), 0.0) + f0 * specular(n, l, v, shin, rough));
    }

    var col = lit + mesh.emissive.rgb;
    var alpha = mesh.base.a;
    // params.w carries the "has texture" flag; the Rust side binds the right texture.
    if (mesh.params.w > 0.5) {
        let t = textureSample(tex, samp, in.uv);
        col = col * t.rgb;
        alpha = alpha * t.a;
    }
    return vec4<f32>(col, alpha);
}
