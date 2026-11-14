//! v1 forward 3D pass: Blinn-Phong with one directional + ambient light.
//!
//! One uniform buffer carries a per-scene frame block (view-projection, camera,
//! lights) followed by a per-mesh block (model + normal matrix, material). The CPU
//! rewrites the whole buffer per mesh — scenes are small in v1, so no dynamic
//! offsets or descriptor churn. Deferred: shadow maps, PBR texture sampling,
//! transparent sorting (output is opaque).

struct Frame {
    view_proj: mat4x4<f32>,
    cam_pos: vec4<f32>,
    // Direction the key light travels, already normalized on the CPU.
    light_dir: vec4<f32>,
    light_color: vec4<f32>,
    ambient: vec4<f32>,
};

struct MeshU {
    model: mat4x4<f32>,
    // Inverse-transpose of model (CPU); keeps normals correct under non-uniform scale.
    normal_mat: mat4x4<f32>,
    base: vec4<f32>,
    emissive: vec4<f32>,
    // metal, roughness, shininess, unused.
    params: vec4<f32>,
};

@group(0) @binding(0) var<uniform> frame: Frame;
@group(0) @binding(1) var<uniform> mesh: MeshU;

struct VSOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) world: vec3<f32>,
    @location(1) normal: vec3<f32>,
};

@vertex
fn vs_main(@location(0) pos: vec3<f32>, @location(1) nrm: vec3<f32>) -> VSOut {
    let world = mesh.model * vec4<f32>(pos, 1.0);
    var out: VSOut;
    out.world = world.xyz;
    out.normal = normalize((mesh.normal_mat * vec4<f32>(nrm, 0.0)).xyz);
    out.clip = frame.view_proj * world;
    return out;
}

@fragment
fn fs_main(in: VSOut) -> @location(0) vec4<f32> {
    let n = normalize(in.normal);
    let v = normalize(frame.cam_pos.xyz - in.world);
    let l = normalize(-frame.light_dir.xyz);
    let metal = mesh.params.x;
    let rough = mesh.params.y;
    let shin = mesh.params.z;

    let diff = max(dot(n, l), 0.0);
    let h = normalize(l + v);
    // F0: dielectric 0.04, metals tint toward the base color (cheap F0 lerp, v1).
    let f0 = mix(vec3<f32>(0.04), mesh.base.rgb, metal);
    let spec = pow(max(dot(n, h), 0.0), shin) * (1.0 - rough * 0.9);

    let diffuse = mesh.base.rgb * (1.0 - metal * 0.85) * (frame.ambient.rgb + frame.light_color.rgb * diff);
    let col = diffuse + f0 * spec * frame.light_color.rgb + mesh.emissive.rgb;
    return vec4<f32>(col, 1.0);
}
