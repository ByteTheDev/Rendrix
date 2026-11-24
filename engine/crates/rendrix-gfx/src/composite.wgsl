// Offscreen layer compositor. A layer's children are rasterised into a transparent RGBA8
// texture; this pass composites that texture over the frame, optionally sampling the
// backdrop that was painted before it. All sampling uses explicit LOD so the shader is
// safe next to the polygon `discard` (which makes control flow non-uniform).
//
// Blur is separable: `fs_blur_h` runs a horizontal 1-D Gaussian into a layer-sized
// intermediate target, then `fs_main` samples that intermediate with the matching
// vertical 1-D Gaussian. This is 7+7 taps instead of a 7x7 (49-tap) product. Taps sit
// at half-integer multiples of `texel * sigma` (symmetric +/-3.5 sigma): a tap grid
// aligned on whole sigmas over-weights a sample exactly one sigma from a hard edge,
// while the half-texel offsets give the correct Gaussian profile.
//
// Backdrop-filter layers are drawn with a REPLACE blend target: the framebuffer already
// contains the *unfiltered* backdrop, so adding a filtered copy of it a second time
// (the old `out + backdrop*(1-out.a)` followed by premultiplied blending) double-applied
// translucent page backgrounds. Instead the shader builds the complete final pixel and
// writes it verbatim, cross-fading with the raw backdrop only at antialiased edges.
// Opaque backdrops produce bit-identical output because the filtered copy has alpha 1
// and fully replaces the destination there.

struct LayerU {
    viewport: vec4<f32>,       // xy = target size in device px; z = blur sigma (blur pass); w = mode/source
    layer_texel: vec4<f32>,    // xy = 1 / layer texture size (also the blur source texel)
    backdrop_texel: vec4<f32>, // xy = 1 / backdrop texture size
    rect: vec4<f32>,           // layer bounds in device px
    params: vec4<f32>,         // opacity, content sigma, backdrop sigma, corner radius
    fx: vec4<f32>,         // grayscale, brightness, has-backdrop, polygon point count
    fx2: vec4<f32>,            // xy = backdrop grayscale / brightness from backdrop-filter
};

// `viewport.w` selects the compositing mode in fs_main:
//   0.0 = premultiplied-alpha blend (normal layers, unchanged byte-for-byte)
//   1.0 = REPLACE blend, backdrop already resolved into `out`
//   2.0 = REPLACE blend, plain copy (blit / no edge cross-fade)
const MODE_PREMUL: f32 = 0.0;
const MODE_REPLACE_BACKDROP: f32 = 1.0;
const MODE_REPLACE_PLAIN: f32 = 2.0;

@group(0) @binding(0) var<uniform> u: LayerU;
// Uniform rather than storage: WebGL2 has no SSBOs. vec4 gives the 16-byte
// uniform-array stride WGSL requires; only .xy is used.
@group(0) @binding(1) var<uniform> poly: array<vec4<f32>, 64>;
@group(1) @binding(0) var layer_tex: texture_2d<f32>;
@group(1) @binding(1) var backdrop_tex: texture_2d<f32>;
@group(1) @binding(2) var raw_backdrop_tex: texture_2d<f32>;
@group(1) @binding(3) var samp: sampler;

struct VOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs_main(@builtin(vertex_index) vi: u32) -> VOut {
    let corner = vec2<f32>(f32(vi & 1u), f32((vi >> 1u) & 1u));
    let p = u.rect.xy + corner * u.rect.zw;
    var o: VOut;
    o.pos = vec4<f32>(p.x / u.viewport.x * 2.0 - 1.0, 1.0 - p.y / u.viewport.y * 2.0, 0.0, 1.0);
    o.uv = corner;
    return o;
}

// Full-screen blit vertex: three vertices cover the viewport with exact 1:1 UVs.
@vertex
fn vs_blit(@builtin(vertex_index) vi: u32) -> VOut {
    let p = array<vec2<f32>, 3>(vec2<f32>(-1.0, -1.0), vec2<f32>(3.0, -1.0), vec2<f32>(-1.0, 3.0))[vi];
    var o: VOut;
    o.pos = vec4<f32>(p, 0.0, 1.0);
    o.uv = vec2<f32>((p.x + 1.0) * 0.5, 1.0 - (p.y + 1.0) * 0.5);
    return o;
}

@fragment
fn fs_blit(v: VOut) -> @location(0) vec4<f32> {
    return textureSampleLevel(layer_tex, samp, v.uv, 0.0);
}

// Horizontal half of the separable Gaussian. `viewport.z` = sigma in source texels,
// `viewport.w` = 0 to sample layer_tex, 1 to sample backdrop_tex. `layer_texel.xy`
// carries the source texture's texel size. Explicit LOD keeps the discard-safe contract.
@fragment
fn fs_blur_h(v: VOut) -> @location(0) vec4<f32> {
    let sigma = u.viewport.z;
    if (sigma <= 0.05) {
        if (u.viewport.w > 0.5) {
            return textureSampleLevel(backdrop_tex, samp, v.uv, 0.0);
        }
        return textureSampleLevel(layer_tex, samp, v.uv, 0.0);
    }
    let step = max(u.layer_texel.xy * sigma, u.layer_texel.xy);
    var sum = vec4<f32>(0.0);
    var wsum = 0.0;
    // Taps at -3.5 .. +3.5 sigma in one-sigma steps (half-integer offsets).
    for (var x = -4; x <= 3; x = x + 1) {
        let o = f32(x) + 0.5;
        let w = exp(-o * o * 0.5);
        let uv = v.uv + vec2<f32>(o * step.x, 0.0);
        var c: vec4<f32>;
        if (u.viewport.w > 0.5) {
            c = textureSampleLevel(backdrop_tex, samp, uv, 0.0);
        } else {
            c = textureSampleLevel(layer_tex, samp, uv, 0.0);
        }
        sum = sum + c * w;
        wsum = wsum + w;
    }
    return sum / wsum;
}

// Vertical half of the separable Gaussian, applied to the horizontal intermediate.
// With sigma ~ 0 this is a plain LOD-0 sample (the no-blur fast path).
fn blur_v(tex: texture_2d<f32>, uv: vec2<f32>, texel: vec2<f32>, sigma: f32) -> vec4<f32> {
    if (sigma <= 0.05) {
        return textureSampleLevel(tex, samp, uv, 0.0);
    }
    let step = max(texel.y * sigma, texel.y);
    var sum = vec4<f32>(0.0);
    var wsum = 0.0;
    for (var y = -4; y <= 3; y = y + 1) {
        let o = f32(y) + 0.5;
        let w = exp(-o * o * 0.5);
        sum = sum + textureSampleLevel(tex, samp, uv + vec2<f32>(0.0, o * step), 0.0) * w;
        wsum = wsum + w;
    }
    return sum / wsum;
}

fn point_in_poly(p: vec2<f32>, n: u32) -> bool {
    if (n < 3u) { return true; }
    var inside = false;
    var j = n - 1u;
    for (var i = 0u; i < n; i = i + 1u) {
        let a = poly[i].xy;
        let b = poly[j].xy;
        if (((a.y > p.y) != (b.y > p.y)) && (p.x < (b.x - a.x) * (p.y - a.y) / (b.y - a.y) + a.x)) {
            inside = !inside;
        }
        j = i;
    }
    return inside;
}

fn pointwise(c: vec4<f32>, g: f32, b: f32) -> vec4<f32> {
    let l = dot(c.rgb, vec3<f32>(0.2126, 0.7152, 0.0722));
    return vec4<f32>(mix(c.rgb, vec3<f32>(l), clamp(g, 0.0, 1.0)) * b, c.a);
}

@fragment
fn fs_main(v: VOut) -> @location(0) vec4<f32> {
    let n = u32(u.fx.w + 0.5);
    // Polygon is stored in device px relative to the layer rect.
    if (!point_in_poly(v.uv * u.rect.zw, n)) { discard; }

    let g = u.fx.x;
    let b = u.fx.y;
    let mode = u.viewport.w;
    // Layer content: horizontally blurred intermediate (or the raw layer when sigma~0)
    // sampled vertically. The pass already carries the content's pointwise filters.
    var out = pointwise(blur_v(layer_tex, v.uv, u.layer_texel.xy, u.params.y), g, b);

    if (u.fx.z > 0.5) {
        let screen_uv = v.pos.xy / u.viewport.xy;
        let backdrop = pointwise(blur_v(backdrop_tex, screen_uv, u.backdrop_texel.xy, u.params.z), u.fx2.x, u.fx2.y);
        // Composite the filtered backdrop under the layer content once. The framebuffer
        // already holds the *unfiltered* backdrop, so this pass uses REPLACE blending and
        // writes the resolved pixel verbatim. `raw_backdrop_tex` is only used for the
        // antialiased rounded-corner cross-fade, never as a second backdrop layer.
        out = out + backdrop * (1.0 - out.a);
        if (mode > 0.5) {
            let raw = textureSampleLevel(raw_backdrop_tex, samp, screen_uv, 0.0);
            let resolved = out * u.params.x;
            let cov = coverage(v);
            if (mode > 1.5) {
                return resolved;
            }
            return mix(raw, resolved, cov);
        }
    }

    // Rounded corners: symmetric SDF in device space.
    let cov = coverage(v);
    return out * u.params.x * cov;
}

fn coverage(v: VOut) -> f32 {
    let half = u.rect.zw * 0.5;
    let center = u.rect.xy + half;
    let r = min(u.params.w, min(half.x, half.y));
    let q = abs(v.pos.xy - center) - half + vec2<f32>(r);
    let d = min(max(q.x, q.y), 0.0) + length(max(q, vec2<f32>(0.0))) - r;
    return clamp(0.5 - d, 0.0, 1.0);
}
