//! The wgpu renderer: flattens a [`DisplayList`] into GPU instances and draws them.

use crate::display::*;
use crate::scene3d::{build_box, build_plane, build_sphere, cloned_scene, Geometry, SceneDraw, Vertex3};
use crate::text::TextSystem;
use bytemuck::{Pod, Zeroable};
use wgpu::util::DeviceExt;
use rendrix_css::animation::{mat_apply, mat_mul, Matrix, IDENTITY};
use rendrix_css::{Color, FilterOp};
use std::collections::HashMap;
use std::num::NonZeroU64;

const ATLAS_SIZE: u32 = 2048;
/// Edge length of the shared image atlas. Small ordinary images are packed here so
/// consecutive `<img>`/background images share one bind group (and therefore one batch).
const IMAGE_ATLAS_SIZE: u32 = 2048;
/// Images with either edge above this stay on the dedicated per-image texture path.
const IMAGE_ATLAS_MAX: u32 = 512;
/// Canvas ids are `0x8000_0000 | node`; they own a dynamic texture and must never be atlased.
const CANVAS_ID_FLAG: ImageId = 0x8000_0000;
const NO_CLIP: [f32; 4] = [-1.0e6, -1.0e6, 2.0e6, 2.0e6];
/// Upper bound on idle offscreen targets retained between frames. Without a cap, a page
/// cycling through many distinct layer sizes grows GPU memory without limit.
const MAX_TARGETS: usize = 32;

/// Round `n` up to the next multiple of `align` (`align` is a power of two).
fn align_to(n: usize, align: usize) -> usize {
    (n + align - 1) / align * align
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable, Default)]
struct Inst {
    rect: [f32; 4],
    xf0: [f32; 4],
    xf1: [f32; 4],
    c: [[f32; 4]; 4],
    stops: [f32; 4],
    radii: [f32; 4],
    border: [f32; 4],
    bcolor: [f32; 4],
    /// Per-side border colours [top, right, bottom, left], premultiplied by alpha.
    bcolors: [[f32; 4]; 4],
    /// Per-side border style codes: 0 solid, 1 dashed, 2 dotted, 3 double.
    bstyle: [f32; 4],
    clip: [f32; 4],
    uv: [f32; 4],
    extra: [f32; 4],
    /// Filter/clip-shape factors, always present so filtered and unfiltered draws share
    /// one pipeline (zero batch impact when unused): `x` = clip kind (0 = rounded rect,
    /// 1 = ellipse inscribed in `clip`), `y` = grayscale 0..1, `z` = brightness factor.
    clip_info: [f32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct Globals {
    viewport: [f32; 2],
    pad: [f32; 2],
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Tex {
    #[allow(dead_code)]
    White,
    Atlas,
    /// The shared image atlas (many small images in one texture, one bind group).
    ImageAtlas,
    Image(ImageId),
}

struct Batch {
    tex: Tex,
    start: u32,
    end: u32,
}

struct ImageTex {
    bind: wgpu::BindGroup,
    #[allow(dead_code)]
    texture: wgpu::Texture,
    pub width: u32,
    pub height: u32,
}

/// A shared RGBA8 texture packing many small images into one GPU texture. Consecutive
/// image draws can then share a bind group and merge into a single batch, instead of
/// paying one batch per visible image. Larger (or atlas-full) images keep the
/// dedicated per-image texture path; canvases always do (they have a dynamic upload).
struct ImageAtlas {
    texture: wgpu::Texture,
    bind: wgpu::BindGroup,
    size: u32,
    /// Allocated slot `(x, y, w, h)` per image id, where `w`/`h` are the interior
    /// image size in pixels. Each slot has a 1px gutter (see `alloc`) so LINEAR
    /// sampling at an edge never bleeds into a neighbour.
    slots: HashMap<ImageId, (u32, u32, u32, u32)>,
    cursor_x: u32,
    cursor_y: u32,
    row_h: u32,
}

impl ImageAtlas {
    /// Reserve a slot for `id`, reusing the existing slot when the size is unchanged
    /// (so a re-upload overwrites in place). Returns the slot's top-left atlas pixel.
    /// `None` means the image does not fit; the caller falls back to a dedicated
    /// texture. A different-size re-upload migrates to a fresh slot; the old pixels
    /// stay in the atlas but are no longer referenced by any instance.
    fn alloc(&mut self, id: ImageId, w: u32, h: u32) -> Option<(u32, u32)> {
        if let Some(&(x, y, sw, sh)) = self.slots.get(&id) {
            if sw == w && sh == h {
                return Some((x, y));
            }
        }
        let (need_w, need_h) = (w + 2, h + 2); // 1px gutter on all four sides
        if need_w > self.size || need_h > self.size {
            return None;
        }
        if self.cursor_x + need_w > self.size {
            self.cursor_x = 0;
            self.cursor_y += self.row_h;
            self.row_h = 0;
        }
        if self.cursor_y + need_h > self.size {
            return None;
        }
        let (x, y) = (self.cursor_x, self.cursor_y);
        self.cursor_x += need_w;
        self.row_h = self.row_h.max(need_h);
        self.slots.insert(id, (x, y, w, h));
        Some((x, y))
    }
}

#[derive(Clone, Copy)]
struct AtlasEntry {
    x: u32,
    y: u32,
    w: u32,
    h: u32,
    left: i32,
    top: i32,
    is_color: bool,
}

struct Atlas {
    texture: wgpu::Texture,
    bind: wgpu::BindGroup,
    size: u32,
    map: HashMap<cosmic_text::CacheKey, Option<AtlasEntry>>,
    /// Last-use tick per resident glyph (blank markers excluded: they occupy no
    /// texture space). Drives LRU eviction in `begin_frame`.
    recency: HashMap<cosmic_text::CacheKey, u64>,
    /// Monotonic tick bumped on every atlas lookup.
    clock: u64,
    cursor_x: u32,
    cursor_y: u32,
    row_h: u32,
    pub evictions: u32,
    /// Bumped every time the atlas is recycled. Instances always reference the
    /// current generation's placements: the recycle is deferred to the next frame
    /// start, so no in-flight instance can ever point at a recycled slot.
    pub generation: u64,
    /// Set when a frame needed more space than the atlas holds; recycled (and
    /// counted) at the next frame start.
    overflowed: bool,
}

struct State {
    xf: Vec<Matrix>,
    clip: Vec<ClipEntry>,
    opacity: Vec<f32>,
    filter: Vec<Vec<FilterOp>>,
    /// `backdrop-filter` levels. Stored for push/pop balance only: sampling the backdrop
    /// needs an offscreen feedback pass, so the ops have no visual effect (documented approx).
    backdrop: Vec<Vec<FilterOp>>,
}

/// One clip-stack level: the intersected rectangle plus the newest level's shape.
/// Per-pixel testing follows the newest level only (outer shapes still cull through the
/// intersected rectangle) — the same approximation overflow clips already used.
#[derive(Clone, Copy)]
struct ClipEntry {
    rect: [f32; 4],
    radius: f32,
    ellipse: bool,
}

// ---- Offscreen compositing layers -----------------------------------------------------
//
// A layer is rendered into its own RGBA8 target and composited as a single quad. This is the
// only path that can read already-rasterised pixels, so `backdrop-filter`, true `filter:
// blur()`, per-pixel `clip-path: polygon()` and isolated opacity use it. Frames without a
// `PushLayer` never touch this machinery — the one-pipeline instanced path is unchanged.

/// An offscreen render target, reusable across frames of the same size.
struct TargetTex {
    #[allow(dead_code)] // kept alive with the view/bind group for the pool's lifetime
    texture: wgpu::Texture,
    view: wgpu::TextureView,
    #[allow(dead_code)] // bind group for the image path if a layer is reused as a texture
    bind: wgpu::BindGroup,
    width: u32,
    height: u32,
    /// Stable identity, used to validate a cached composite bind group across frames.
    uid: u64,
    /// Last pool tick at which this target was acquired/returned (LRU eviction).
    last_use: u64,
    /// Cached composite texture bind group plus the uids of the three textures it binds.
    /// Reused whenever a later layer binds the same trio (typically the same backdrop
    /// accumulator across frames), avoiding a fresh descriptor allocation per layer.
    comp_bind: Option<([u64; 3], wgpu::BindGroup)>,
}

/// Fixed-size uniform block for the composite shader (7 vec4s, 112 bytes).
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct LayerU {
    viewport: [f32; 4],       // xy = target size in device px
    layer_texel: [f32; 4],    // xy = 1 / layer texture size
    backdrop_texel: [f32; 4], // xy = 1 / backdrop texture size
    rect: [f32; 4],           // layer bounds in device px (x, y, w, h)
    params: [f32; 4],         // opacity, content sigma, backdrop sigma, radius (device px)
    filter: [f32; 4],         // grayscale, brightness, has-backdrop (0/1), polygon point count
    fx2: [f32; 4],            // xy = backdrop grayscale / brightness (backdrop-filter ops)
}

/// Per-draw-chunk GPU state. Every pass of a top-level frame is recorded into a single
/// command encoder and submitted once, so all `queue.write_buffer` calls land before that
/// submit. Two flat runs sharing one instance buffer would therefore both draw the *last*
/// chunk's instances; each chunk owns its instance/3D-uniform buffers to avoid that.
struct DrawSlot {
    inst_buf: wgpu::Buffer,
    inst_cap: usize,
    /// Per-chunk globals: a nested layer render has a different viewport, so sharing
    /// this buffer would make the last chunk's viewport win for every pass.
    globals_buf: wgpu::Buffer,
    globals_bind: wgpu::BindGroup,
    td_frame_buf: wgpu::Buffer,
    td_mesh_buf: wgpu::Buffer,
    td_bind: wgpu::BindGroup,
}

const MAX_POLY_POINTS: usize = 64;

struct Composite {
    pipeline: wgpu::RenderPipeline,
    /// REPLACE-blend variant used for backdrop-filter layers: the shader resolves the
    /// backdrop itself, so the old destination must not be blended in a second time.
    replace_pipeline: wgpu::RenderPipeline,
    /// Horizontal Gaussian pass into a layer-sized intermediate target.
    blur_pipeline: wgpu::RenderPipeline,
    /// Exact 1:1 copy of the finished offscreen frame into the caller's target view.
    blit_pipeline: wgpu::RenderPipeline,
    #[allow(dead_code)] // owned so the uniform bind group keeps a valid layout
    u_layout: wgpu::BindGroupLayout,
    t_layout: wgpu::BindGroupLayout,
    u_buf: wgpu::Buffer,
    u_bind: wgpu::BindGroup,
    sampler: wgpu::Sampler,
    /// One uniform/polygon buffer pair per GPU pass in the current frame. Distinct slots
    /// let every layer's blur/composite passes share a single encoder and submit: all the
    /// `write_buffer` calls land before the submit, so each pass must own its bytes.
    slots: Vec<PassSlot>,
    slot_used: usize,
}

struct PassSlot {
    u_buf: wgpu::Buffer,
    poly_buf: wgpu::Buffer,
    bind: wgpu::BindGroup,
}

/// A slice of the display list to draw into one target, with any ancestor transform/clip
/// markers that were open at that point replayed around it so flatten state is preserved.
struct FlatSegment {
    items: Vec<Item>,
}

/// A planned offscreen layer: the layer's own subtree plus the ancestor markers that were
/// open at its boundary. The painted prefix is captured by a single frame snapshot instead
/// of being copied into each layer plan.
enum Segment {
    Flat(FlatSegment),
    Layer {
        info: LayerInfo,
        open: Vec<Item>,
        children: Vec<Item>,
    },
}

fn make_composite(device: &wgpu::Device, format: wgpu::TextureFormat) -> Composite {
    let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("rendrix composite shader"),
        source: wgpu::ShaderSource::Wgsl(include_str!("composite.wgsl").into()),
    });
    let u_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("rendrix composite uniforms"),
        entries: &[
            wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                ty: wgpu::BindingType::Buffer { ty: wgpu::BufferBindingType::Uniform, has_dynamic_offset: false, min_binding_size: None },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 1,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Buffer { ty: wgpu::BufferBindingType::Storage { read_only: true }, has_dynamic_offset: false, min_binding_size: None },
                count: None,
            },
        ],
    });
    let t_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("rendrix composite textures"),
        entries: &[
            wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture { sample_type: wgpu::TextureSampleType::Float { filterable: true }, view_dimension: wgpu::TextureViewDimension::D2, multisampled: false },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 1,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture { sample_type: wgpu::TextureSampleType::Float { filterable: true }, view_dimension: wgpu::TextureViewDimension::D2, multisampled: false },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 2,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture { sample_type: wgpu::TextureSampleType::Float { filterable: true }, view_dimension: wgpu::TextureViewDimension::D2, multisampled: false },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 3,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                count: None,
            },
        ],
    });
    let pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("rendrix composite layout"),
        bind_group_layouts: &[&u_layout, &t_layout],
        push_constant_ranges: &[],
    });
    let make_pipeline = |label: &'static str, vs: &'static str, fs: &'static str, blend: Option<wgpu::BlendState>, topology: wgpu::PrimitiveTopology| {
        device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some(label),
            layout: Some(&pl),
            vertex: wgpu::VertexState { module: &module, entry_point: Some(vs), compilation_options: Default::default(), buffers: &[] },
            fragment: Some(wgpu::FragmentState {
                module: &module,
                entry_point: Some(fs),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState { format, blend, write_mask: wgpu::ColorWrites::ALL })],
            }),
            primitive: wgpu::PrimitiveState { topology, ..Default::default() },
            depth_stencil: None,
            multisample: Default::default(),
            multiview: None,
            cache: None,
        })
    };
    let strip = wgpu::PrimitiveTopology::TriangleStrip;
    let pipeline = make_pipeline("rendrix composite pipeline", "vs_main", "fs_main", Some(wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING), strip);
    let replace_pipeline = make_pipeline("rendrix composite replace", "vs_main", "fs_main", Some(wgpu::BlendState::REPLACE), strip);
    let blur_pipeline = make_pipeline("rendrix composite blur-h", "vs_main", "fs_blur_h", Some(wgpu::BlendState::REPLACE), strip);
    let blit_pipeline = make_pipeline("rendrix composite blit", "vs_blit", "fs_blit", Some(wgpu::BlendState::REPLACE), wgpu::PrimitiveTopology::TriangleList);
    let u_buf = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("rendrix composite uniforms"),
        size: std::mem::size_of::<LayerU>() as u64,
        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let poly_buf = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("rendrix composite polygon"),
        size: (MAX_POLY_POINTS * 8) as u64,
        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let u_bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("rendrix composite uniforms bind"),
        layout: &u_layout,
        entries: &[
            wgpu::BindGroupEntry { binding: 0, resource: u_buf.as_entire_binding() },
            wgpu::BindGroupEntry { binding: 1, resource: poly_buf.as_entire_binding() },
        ],
    });
    let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
        mag_filter: wgpu::FilterMode::Linear,
        min_filter: wgpu::FilterMode::Linear,
        address_mode_u: wgpu::AddressMode::ClampToEdge,
        address_mode_v: wgpu::AddressMode::ClampToEdge,
        ..Default::default()
    });
    Composite { pipeline, replace_pipeline, blur_pipeline, blit_pipeline, u_layout, t_layout, u_buf, u_bind, sampler, slots: Vec::new(), slot_used: 0 }
}

/// Order resident glyphs newest-first for LRU repack (pure helper, unit-tested).
fn lru_order(recency: &HashMap<cosmic_text::CacheKey, u64>) -> Vec<(cosmic_text::CacheKey, u64)> {
    let mut v: Vec<_> = recency.iter().map(|(k, t)| (*k, *t)).collect();
    v.sort_by_key(|(_, t)| std::cmp::Reverse(*t));
    v
}

/// Conservative offscreen test for a whole text item in device space.
/// The paragraph height is unknown pre-shape, so only left/right/below are
/// culled; anything possibly visible returns true. Pure helper, unit-tested.
fn text_maybe_visible(x: f32, y: f32, w: f32, clip: [f32; 4]) -> bool {
    !(x > clip[0] + clip[2] || x + w < clip[0] || y > clip[1] + clip[3])
}

// ---- 3D forward pass (R6.2 v1) ------------------------------------------------------
//
// One depth-tested pipeline coexists with the 2D SDF pass: the 2D pass draws first,
// then each `SceneDraw` viewport composites on top with viewport+scissor set to the
// CSS layout rect. Lighting is Blinn-Phong with a single directional + ambient
// (see scene3d.wgsl); transparency, shadows and textures are deferred.

/// Interleaved 3D vertex: position + normal.
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct V3 {
    pos: [f32; 3],
    nrm: [f32; 3],
}

impl From<Vertex3> for V3 {
    fn from(v: Vertex3) -> Self {
        V3 { pos: v.pos, nrm: v.normal }
    }
}

/// Per-scene block of the 3D uniform buffer.
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct FrameU {
    view_proj: [[f32; 4]; 4],
    cam_pos: [f32; 4],
    light_dir: [f32; 4],
    light_color: [f32; 4],
    ambient: [f32; 4],
}

/// Per-mesh block of the 3D uniform buffer.
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct MeshU {
    model: [[f32; 4]; 4],
    normal_mat: [[f32; 4]; 4],
    base: [f32; 4],
    emissive: [f32; 4],
    /// metal, roughness, shininess, unused.
    params: [f32; 4],
}

struct GpuMesh {
    vbuf: wgpu::Buffer,
    ibuf: wgpu::Buffer,
    count: u32,
}

pub struct Renderer {
    format: wgpu::TextureFormat,
    device: wgpu::Device,
    queue: wgpu::Queue,
    pipeline: wgpu::RenderPipeline,
    #[allow(dead_code)] // template buffer retained for renderer compatibility
    globals_buf: wgpu::Buffer,
    #[allow(dead_code)] // template bind group retained for renderer compatibility
    globals_bind: wgpu::BindGroup,
    g_layout: wgpu::BindGroupLayout,
    tex_layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    white_bind: wgpu::BindGroup,
    /// View of the 1x1 white texture, used as a neutral backdrop/mask in layer compositing.
    white_view: wgpu::TextureView,
    atlas: Atlas,
    /// Shared texture for small decoded images (one bind group, batched draws).
    image_atlas: ImageAtlas,
    images: HashMap<ImageId, ImageTex>,
    next_image: ImageId,
    #[allow(dead_code)] // template buffer retained for renderer compatibility
    inst_buf: wgpu::Buffer,
    inst_cap: usize,
    /// Number of instances drawn in the last frame (profiling).
    pub last_instances: usize,
    pub last_batches: usize,
    // ---- 3D forward pass (R6.2 v1) -------------------------------------------------
    td_pipeline: wgpu::RenderPipeline,
    /// Kept so the 3D bind group can be rebuilt when the uniform buffers grow.
    td_layout: wgpu::BindGroupLayout,
    td_bind: wgpu::BindGroup,
    td_frame_buf: wgpu::Buffer,
    td_mesh_buf: wgpu::Buffer,
    /// Dynamic uniform offset alignment for this device (at least 256 bytes).
    td_align: usize,
    td_box: GpuMesh,
    td_plane: GpuMesh,
    td_sphere: GpuMesh,
    /// Raw-triangle meshes keyed by (scene_id, mesh_index, scene_revision).
    td_tris: HashMap<(u64, usize, u64), GpuMesh>,
    depth: Option<(u32, u32, wgpu::Texture)>,
    /// Target size of the last `render` call (for standalone 3D compositing).
    last_size: (u32, u32),
    /// 3D meshes drawn in the last frame (profiling).
    pub last_3d_meshes: usize,
    // ---- Offscreen compositing layers -------------------------------------------------
    comp: Composite,
    /// Idle per-chunk draw state, reused across frames.
    draw_slots: Vec<DrawSlot>,
    /// Slots used by the frame currently being recorded. They must not return to the
    /// idle pool until after the single submit, or a later chunk would overwrite the
    /// instance/3D uniforms referenced by an earlier pass in the same encoder.
    active_draw_slots: Vec<DrawSlot>,
    targets: Vec<TargetTex>,
    /// Monotonic tick for target-pool LRU bookkeeping.
    target_clock: u64,
    /// Monotonic id assigned to every offscreen target (bind-group cache signature).
    next_target_uid: u64,
    /// Number of offscreen layer passes in the last frame (profiling).
    pub last_layers: usize,
    /// True while rendering a layer's children/backdrop (nested renders must not reset counters).
    rendering_layer: bool,
}

impl Renderer {
    /// One pipeline draws everything; batches only split on texture switches.
    /// The premultiplied-alpha blend contract lives in the pipeline descriptor
    /// below and in the shader — never add a second pipeline or blend mode.
    pub fn new(device: wgpu::Device, queue: wgpu::Queue, format: wgpu::TextureFormat) -> Renderer {
        Self::with_atlas_size(device, queue, format, ATLAS_SIZE)
    }

    /// Same renderer with an explicit glyph-atlas edge length (default 2048).
    /// Exists for overflow-recovery tests; production keeps the default.
    pub fn with_atlas_size(device: wgpu::Device, queue: wgpu::Queue, format: wgpu::TextureFormat, atlas_size: u32) -> Renderer {
        let atlas_size = atlas_size.max(16);
        let comp = make_composite(&device, format);
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("rendrix shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shader.wgsl").into()),
        });
        let g_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("rendrix globals"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                    ty: wgpu::BindingType::Buffer { ty: wgpu::BufferBindingType::Uniform, has_dynamic_offset: false, min_binding_size: None },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                    ty: wgpu::BindingType::Buffer { ty: wgpu::BufferBindingType::Storage { read_only: true }, has_dynamic_offset: false, min_binding_size: None },
                    count: None,
                },
            ],
        });
        let tex_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("rendrix texture"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
        let pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor { label: Some("rendrix layout"), bind_group_layouts: &[&g_layout, &tex_layout], push_constant_ranges: &[] });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("rendrix pipeline"),
            layout: Some(&pl),
            vertex: wgpu::VertexState { module: &module, entry_point: Some("vs_main"), compilation_options: Default::default(), buffers: &[] },
            fragment: Some(wgpu::FragmentState {
                module: &module,
                entry_point: Some("fs_main"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState { format, blend: Some(wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING), write_mask: wgpu::ColorWrites::ALL })],
            }),
            primitive: wgpu::PrimitiveState { topology: wgpu::PrimitiveTopology::TriangleStrip, ..Default::default() },
            depth_stencil: None,
            multisample: Default::default(),
            multiview: None,
            cache: None,
        });
        let globals_buf = device.create_buffer(&wgpu::BufferDescriptor { label: Some("rendrix globals"), size: 16, usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false });
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            ..Default::default()
        });
        let make_tex = |w: u32, h: u32, label: &str| {
            device.create_texture(&wgpu::TextureDescriptor {
                label: Some(label),
                size: wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba8Unorm,
                usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                view_formats: &[],
            })
        };
        let bind_for = |tex: &wgpu::Texture| {
            let view = tex.create_view(&Default::default());
            device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("rendrix tex bind"),
                layout: &tex_layout,
                entries: &[wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&view) }, wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::Sampler(&sampler) }],
            })
        };
        let white = make_tex(1, 1, "rendrix white");
        queue.write_texture(
            white.as_image_copy(),
            &[255, 255, 255, 255],
            wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(4), rows_per_image: Some(1) },
            wgpu::Extent3d { width: 1, height: 1, depth_or_array_layers: 1 },
        );
        let white_bind = bind_for(&white);
        let white_view = white.create_view(&Default::default());
        let atlas_tex = make_tex(atlas_size, atlas_size, "rendrix glyph atlas");
        // A 2x2 opaque white block at the atlas origin: shapes sample it, so rects, shadows and glyphs
        // share one texture and consecutive draws merge into a single batch.
        queue.write_texture(
            wgpu::TexelCopyTextureInfo { texture: &atlas_tex, mip_level: 0, origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All },
            &[255u8; 16],
            wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(8), rows_per_image: Some(2) },
            wgpu::Extent3d { width: 2, height: 2, depth_or_array_layers: 1 },
        );
        let atlas_bind = bind_for(&atlas_tex);
        // Separate from the glyph atlas: glyphs are LRU-recycled on overflow, while
        // image slots stay put for the renderer's lifetime (re-uploads overwrite in place).
        let image_atlas_tex = make_tex(IMAGE_ATLAS_SIZE, IMAGE_ATLAS_SIZE, "rendrix image atlas");
        let image_atlas_bind = bind_for(&image_atlas_tex);
        let inst_cap = 1024;
        let inst_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("rendrix instances"),
            size: (inst_cap * std::mem::size_of::<Inst>()) as u64,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let globals_bind = Self::make_globals_bind(&device, &g_layout, &globals_buf, &inst_buf);
        // ---- 3D forward pass --------------------------------------------------
        let td_module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("rendrix 3d shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("scene3d.wgsl").into()),
        });
        // One slot per scene and per mesh, addressed with dynamic offsets. All draws
        // share a single encoder/submit, so rewriting a single buffer slot per draw
        // would make the last scene/mesh win for *every* draw. Uniform offsets must
        // be aligned to the device limit (>= 256 bytes).
        let td_align = (device.limits().min_uniform_buffer_offset_alignment as usize).max(256);
        let td_frame_slot = align_to(std::mem::size_of::<FrameU>(), td_align) as u64;
        let td_mesh_slot = align_to(std::mem::size_of::<MeshU>(), td_align) as u64;
        let td_frame_slot_nz = NonZeroU64::new(td_frame_slot).unwrap();
        let td_mesh_slot_nz = NonZeroU64::new(td_mesh_slot).unwrap();
        let td_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("rendrix 3d uniforms"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                    ty: wgpu::BindingType::Buffer { ty: wgpu::BufferBindingType::Uniform, has_dynamic_offset: true, min_binding_size: Some(td_frame_slot_nz) },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                    ty: wgpu::BindingType::Buffer { ty: wgpu::BufferBindingType::Uniform, has_dynamic_offset: true, min_binding_size: Some(td_mesh_slot_nz) },
                    count: None,
                },
            ],
        });
        let td_frame_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("rendrix 3d frame"),
            size: td_frame_slot,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let td_mesh_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("rendrix 3d mesh"),
            size: td_mesh_slot,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let td_bind = Renderer::make_td_bind(&device, &td_layout, &td_frame_buf, &td_mesh_buf, td_frame_slot_nz, td_mesh_slot_nz);
        let td_pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor { label: Some("rendrix 3d layout"), bind_group_layouts: &[&td_layout], push_constant_ranges: &[] });
        let td_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("rendrix 3d pipeline"),
            layout: Some(&td_pl),
            vertex: wgpu::VertexState {
                module: &td_module,
                entry_point: Some("vs_main"),
                compilation_options: Default::default(),
                buffers: &[wgpu::VertexBufferLayout {
                    array_stride: std::mem::size_of::<V3>() as u64,
                    step_mode: wgpu::VertexStepMode::Vertex,
                    attributes: &wgpu::vertex_attr_array![0 => Float32x3, 1 => Float32x3],
                }],
            },
            fragment: Some(wgpu::FragmentState {
                module: &td_module,
                entry_point: Some("fs_main"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState { format, blend: Some(wgpu::BlendState::REPLACE), write_mask: wgpu::ColorWrites::ALL })],
            }),
            // No face culling in v1: procedural shapes and imported triangle soup do
            // not share one winding convention; low-poly scenes stay cheap anyway.
            primitive: wgpu::PrimitiveState { topology: wgpu::PrimitiveTopology::TriangleList, cull_mode: None, front_face: wgpu::FrontFace::Ccw, ..Default::default() },
            depth_stencil: Some(wgpu::DepthStencilState {
                format: wgpu::TextureFormat::Depth24Plus,
                depth_write_enabled: true,
                depth_compare: wgpu::CompareFunction::Less,
                stencil: Default::default(),
                bias: Default::default(),
            }),
            multisample: Default::default(),
            multiview: None,
            cache: None,
        });
        let gpu_mesh = |verts: &[Vertex3], idx: &[u32], label: &str| {
            let v: Vec<V3> = verts.iter().copied().map(V3::from).collect();
            let vbuf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor { label: Some(label), contents: bytemuck::cast_slice(&v), usage: wgpu::BufferUsages::VERTEX });
            let ibuf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor { label: Some(label), contents: bytemuck::cast_slice(idx), usage: wgpu::BufferUsages::INDEX });
            GpuMesh { vbuf, ibuf, count: idx.len() as u32 }
        };
        let (box_v, box_i) = build_box();
        let (plane_v, plane_i) = build_plane();
        let (sph_v, sph_i) = build_sphere();
        let td_box = gpu_mesh(&box_v, &box_i, "rendrix 3d box");
        let td_plane = gpu_mesh(&plane_v, &plane_i, "rendrix 3d plane");
        let td_sphere = gpu_mesh(&sph_v, &sph_i, "rendrix 3d sphere");
        Renderer {
            format,
            device,
            queue,
            pipeline,
            globals_buf,
            globals_bind,
            g_layout,
            tex_layout,
            sampler,
            white_bind,
            white_view,
            atlas: Atlas { texture: atlas_tex, bind: atlas_bind, size: atlas_size, map: HashMap::new(), recency: HashMap::new(), clock: 0, cursor_x: 3, cursor_y: 0, row_h: 3, evictions: 0, generation: 0, overflowed: false },
            image_atlas: ImageAtlas { texture: image_atlas_tex, bind: image_atlas_bind, size: IMAGE_ATLAS_SIZE, slots: HashMap::new(), cursor_x: 0, cursor_y: 0, row_h: 0 },
            images: HashMap::new(),
            next_image: 1,
            inst_buf,
            inst_cap,
            last_instances: 0,
            last_batches: 0,
            td_pipeline,
            td_layout,
            td_bind,
            td_frame_buf,
            td_mesh_buf,
            td_align,
            td_box,
            td_plane,
            td_sphere,
            td_tris: HashMap::new(),
            depth: None,
            last_size: (1, 1),
            last_3d_meshes: 0,
            comp,
            draw_slots: Vec::new(),
            active_draw_slots: Vec::new(),
            targets: Vec::new(),
            target_clock: 0,
            next_target_uid: 1,
            last_layers: 0,
            rendering_layer: false,
        }
    }

    fn make_globals_bind(device: &wgpu::Device, layout: &wgpu::BindGroupLayout, globals_buf: &wgpu::Buffer, inst_buf: &wgpu::Buffer) -> wgpu::BindGroup {
        device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("rendrix globals bind"),
            layout,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: globals_buf.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: inst_buf.as_entire_binding() },
            ],
        })
    }

    #[inline]
    fn td_frame_slot(&self) -> u64 {
        align_to(std::mem::size_of::<FrameU>(), self.td_align) as u64
    }

    #[inline]
    fn td_mesh_slot(&self) -> u64 {
        align_to(std::mem::size_of::<MeshU>(), self.td_align) as u64
    }

    fn make_td_bind(
        device: &wgpu::Device,
        layout: &wgpu::BindGroupLayout,
        frame_buf: &wgpu::Buffer,
        mesh_buf: &wgpu::Buffer,
        frame_slot: NonZeroU64,
        mesh_slot: NonZeroU64,
    ) -> wgpu::BindGroup {
        device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("rendrix 3d bind"),
            layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding { buffer: frame_buf, offset: 0, size: Some(frame_slot) }),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding { buffer: mesh_buf, offset: 0, size: Some(mesh_slot) }),
                },
            ],
        })
    }

    /// Grow the 3D uniform buffers to hold `frames` frame slots and `meshes` mesh
    /// slots. Buffers (and the bind group referencing them) are only recreated when
    /// capacity is actually exceeded, so steady frames reuse the same GPU memory.
    fn ensure_td_capacity(&mut self, frames: usize, meshes: usize) {
        let (fs, ms) = (self.td_frame_slot(), self.td_mesh_slot());
        let need_f = (frames.max(1) as u64) * fs;
        let need_m = (meshes.max(1) as u64) * ms;
        if self.td_frame_buf.size() >= need_f && self.td_mesh_buf.size() >= need_m {
            return;
        }
        if self.td_frame_buf.size() < need_f {
            let mut cap = self.td_frame_buf.size().max(fs);
            while cap < need_f {
                cap *= 2;
            }
            self.td_frame_buf = self.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("rendrix 3d frame"),
                size: cap,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
        }
        if self.td_mesh_buf.size() < need_m {
            let mut cap = self.td_mesh_buf.size().max(ms);
            while cap < need_m {
                cap *= 2;
            }
            self.td_mesh_buf = self.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("rendrix 3d mesh"),
                size: cap,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
        }
        // The bind group pins the old buffer(s); rebuild it after any regrow.
        self.td_bind = Self::make_td_bind(
            &self.device,
            &self.td_layout,
            &self.td_frame_buf,
            &self.td_mesh_buf,
            NonZeroU64::new(fs).unwrap(),
            NonZeroU64::new(ms).unwrap(),
        );
    }

    /// Glyph-atlas recycles so far (each one LRU-repacked the atlas and re-uploaded).
    pub fn atlas_evictions(&self) -> u32 {
        self.atlas.evictions
    }

    /// Atlas generation: bumped on every eviction; used to detect mid-frame clears.
    pub fn atlas_generation(&self) -> u64 {
        self.atlas.generation
    }

    pub fn device(&self) -> &wgpu::Device {
        &self.device
    }

    pub fn queue(&self) -> &wgpu::Queue {
        &self.queue
    }

    /// Upload straight-alpha RGBA8 pixels; returns a handle for [`ImageItem`].
    pub fn upload_image(&mut self, rgba: &[u8], width: u32, height: u32) -> ImageId {
        let id = self.next_image;
        self.next_image += 1;
        self.upload_image_as(id, rgba, width, height);
        id
    }

    /// Premultiply straight-alpha RGBA8 in place (shared by image and canvas uploads; the
    /// premultiplied-alpha blend contract lives in the pipeline descriptor, never per-texture).
    fn premultiply(rgba: &mut [u8]) {
        for px in rgba.chunks_exact_mut(4) {
            let a = px[3] as u32;
            px[0] = (px[0] as u32 * a / 255) as u8;
            px[1] = (px[1] as u32 * a / 255) as u8;
            px[2] = (px[2] as u32 * a / 255) as u8;
        }
    }

    /// Build a `(w+2)x(h+2)` premultiplied buffer with the edge pixels replicated
    /// into a 1px gutter. LINEAR sampling at an image's border then never mixes in
    /// transparent black or a neighbouring image's pixels.
    fn pad_edges(pm: &[u8], w: u32, h: u32) -> Vec<u8> {
        let (w, h) = (w as usize, h as usize);
        let mut out = vec![0u8; (w + 2) * (h + 2) * 4];
        for j in 0..h + 2 {
            let sj = j.saturating_sub(1).min(h - 1);
            for i in 0..w + 2 {
                let si = i.saturating_sub(1).min(w - 1);
                let src = (sj * w + si) * 4;
                let dst = (j * (w + 2) + i) * 4;
                out[dst..dst + 4].copy_from_slice(&pm[src..src + 4]);
            }
        }
        out
    }

    /// Upload under a caller-chosen id (the engine owns id allocation).
    pub fn upload_image_as(&mut self, id: ImageId, rgba: &[u8], width: u32, height: u32) {
        let mut pm = rgba.to_vec();
        Self::premultiply(&mut pm);
        // Canvas ids (high bit set) own a dynamic texture that `upload_canvas` /
        // `update_canvas_region` write into directly, so they must keep the dedicated
        // texture path. Small ordinary images share the image atlas; oversized images
        // and images that no longer fit use the one-texture-per-image fallback.
        if id & CANVAS_ID_FLAG == 0 && width > 0 && height > 0
            && width <= IMAGE_ATLAS_MAX && height <= IMAGE_ATLAS_MAX
        {
            if let Some((x, y)) = self.image_atlas.alloc(id, width, height) {
                let padded = Self::pad_edges(&pm, width, height);
                self.queue.write_texture(
                    wgpu::TexelCopyTextureInfo { texture: &self.image_atlas.texture, mip_level: 0, origin: wgpu::Origin3d { x, y, z: 0 }, aspect: wgpu::TextureAspect::All },
                    &padded,
                    wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some((width + 2) * 4), rows_per_image: Some(height + 2) },
                    wgpu::Extent3d { width: width + 2, height: height + 2, depth_or_array_layers: 1 },
                );
                // Drop a stale dedicated texture from an earlier (e.g. larger) upload.
                self.images.remove(&id);
                return;
            }
        }
        // Fallback: canvases, images over IMAGE_ATLAS_MAX, or an atlas with no room
        // left. If this id previously lived in the atlas, invalidate that slot so
        // flattening cannot reference stale pixels.
        self.image_atlas.slots.remove(&id);
        let texture = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("rendrix image"),
            size: wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        self.queue.write_texture(
            texture.as_image_copy(),
            &pm,
            wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(width * 4), rows_per_image: Some(height) },
            wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
        );
        let view = texture.create_view(&Default::default());
        let bind = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("rendrix image bind"),
            layout: &self.tex_layout,
            entries: &[wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&view) }, wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::Sampler(&self.sampler) }],
        });
        self.images.insert(id, ImageTex { bind, texture, width, height });
    }

    pub fn image_size(&self, id: ImageId) -> Option<(u32, u32)> {
        self.images.get(&id).map(|i| (i.width, i.height))
            .or_else(|| self.image_atlas.slots.get(&id).map(|&(_, _, w, h)| (w, h)))
    }

    pub fn free_image(&mut self, id: ImageId) {
        self.images.remove(&id);
        self.image_atlas.slots.remove(&id);
    }

    // ---- Canvas 2D targets (A3) ------------------------------------------------------------
    //
    // One GPU texture per `<canvas>`, keyed by `canvas_image_id(node)` so ids never collide
    // with decoded images (which count up from 1). Managed by the platform shell: create on
    // first draw, recreate when `width`/`height` attributes change, re-upload otherwise.
    // Dirty-region re-upload (`update_canvas_region`) is cheaper than a full re-upload when the
    // union of a frame's draws covers less than roughly half the bitmap; the shell picks via
    // `CanvasUpload::dirty`.

    /// Full upload (or recreate on size change) of a canvas bitmap. Zero-size is a no-op:
    /// zero-area canvases are never composited and own no texture.
    pub fn upload_canvas(&mut self, node: u32, rgba: &[u8], width: u32, height: u32) {
        if width == 0 || height == 0 {
            return;
        }
        let id = crate::display::canvas_image_id(node);
        let same_size = self.images.get(&id).is_some_and(|t| t.width == width && t.height == height);
        if same_size {
            let mut pm = rgba.to_vec();
            Self::premultiply(&mut pm);
            self.queue.write_texture(
                self.images.get(&id).unwrap().texture.as_image_copy(),
                &pm,
                wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(width * 4), rows_per_image: Some(height) },
                wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
            );
        } else {
            self.upload_image_as(id, rgba, width, height);
        }
    }

    /// Re-upload only `region` (`[x, y, w, h]` in pixels) of a same-size canvas bitmap.
    /// Falls back to a full upload when the texture is missing or the size changed.
    pub fn update_canvas_region(&mut self, node: u32, full: &[u8], full_w: u32, full_h: u32, region: [u32; 4]) {
        let id = crate::display::canvas_image_id(node);
        let same = self.images.get(&id).is_some_and(|t| t.width == full_w && t.height == full_h);
        if !same {
            self.upload_canvas(node, full, full_w, full_h);
            return;
        }
        let [x, y, w, h] = region;
        let (w, h) = (w.min(full_w.saturating_sub(x)), h.min(full_h.saturating_sub(y)));
        if w == 0 || h == 0 {
            return;
        }
        let mut pm = Vec::with_capacity((w * h * 4) as usize);
        for row in 0..h {
            let s = ((y + row) * full_w + x) as usize * 4;
            pm.extend_from_slice(&full[s..s + (w * 4) as usize]);
        }
        Self::premultiply(&mut pm);
        self.queue.write_texture(
            wgpu::TexelCopyTextureInfo { texture: &self.images.get(&id).unwrap().texture, mip_level: 0, origin: wgpu::Origin3d { x, y, z: 0 }, aspect: wgpu::TextureAspect::All },
            &pm,
            wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(w * 4), rows_per_image: Some(h) },
            wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
        );
    }

    /// Drop a canvas texture (call when its element leaves the document).
    pub fn remove_canvas(&mut self, node: u32) {
        self.images.remove(&crate::display::canvas_image_id(node));
    }

    /// Current GPU target size, if one exists.
    pub fn canvas_size(&self, node: u32) -> Option<(u32, u32)> {
        self.images.get(&crate::display::canvas_image_id(node)).map(|t| (t.width, t.height))
    }

    // ------------------------------------------------------------------------------------

    fn atlas_entry(&mut self, text: &mut TextSystem, key: cosmic_text::CacheKey) -> Option<AtlasEntry> {
        self.atlas.clock = self.atlas.clock.wrapping_add(1);
        let now = self.atlas.clock;
        if let Some(e) = self.atlas.map.get(&key) {
            if e.is_some() {
                self.atlas.recency.insert(key, now);
            }
            return *e;
        }
        let Some(r) = text.raster(key) else {
            self.atlas.map.insert(key, None);
            return None;
        };
        let (w, h) = (r.width + 1, r.height + 1);
        let size = self.atlas.size;
        if self.atlas.cursor_x + w > size {
            self.atlas.cursor_x = 0;
            self.atlas.cursor_y += self.atlas.row_h;
            self.atlas.row_h = 0;
        }
        if self.atlas.cursor_y + h > size {
            // Full: skip this glyph for the rest of the frame and recycle the
            // atlas at the next frame start. Deferring the clear (instead of
            // clearing here) guarantees no emitted instance references a stale
            // placement; the cost is a one-frame-missing glyph under extreme
            // text storms, never a wrong glyph.
            self.atlas.overflowed = true;
            return None;
        }
        let (x, y) = (self.atlas.cursor_x, self.atlas.cursor_y);
        self.atlas.cursor_x += w;
        self.atlas.row_h = self.atlas.row_h.max(h);
        self.queue.write_texture(
            wgpu::TexelCopyTextureInfo { texture: &self.atlas.texture, mip_level: 0, origin: wgpu::Origin3d { x, y, z: 0 }, aspect: wgpu::TextureAspect::All },
            &r.rgba,
            wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(r.width * 4), rows_per_image: Some(r.height) },
            wgpu::Extent3d { width: r.width, height: r.height, depth_or_array_layers: 1 },
        );
        let e = AtlasEntry { x, y, w: r.width, h: r.height, left: r.left, top: r.top, is_color: r.is_color };
        self.atlas.map.insert(key, Some(e));
        self.atlas.recency.insert(key, now);
        Some(e)
    }

    /// Recycle the glyph atlas if the previous frame overflowed it. Runs at frame
    /// start so in-flight instances never go stale.
    ///
    /// Per-glyph LRU: resident glyphs are re-uploaded newest-first into the fresh
    /// atlas until it is full; the coldest glyphs stay evicted and fault back in
    /// on demand. The 2x2 white block at the origin is never overwritten
    /// (allocation restarts at x=3), so shapes keep sampling it. Blank-glyph
    /// markers occupy no texture space and are always retained.
    fn begin_frame(&mut self, text: &mut TextSystem) {
        if !self.atlas.overflowed {
            return;
        }
        let live = lru_order(&self.atlas.recency);
        let blanks: Vec<cosmic_text::CacheKey> =
            self.atlas.map.iter().filter_map(|(k, v)| v.is_none().then_some(*k)).collect();
        self.atlas.map.clear();
        self.atlas.recency.clear();
        let (_nlive, _nblanks) = (live.len(), blanks.len());
        for k in blanks {
            self.atlas.map.insert(k, None);
        }
        self.atlas.cursor_x = 3;
        self.atlas.cursor_y = 0;
        self.atlas.row_h = 3;
        self.atlas.evictions += 1;
        self.atlas.generation += 1;
        self.atlas.overflowed = false;
        self.atlas.clock = self.atlas.clock.wrapping_add(1);
        for (key, used) in live {
            let Some(r) = text.raster(key) else {
                self.atlas.map.insert(key, None);
                continue;
            };
            let (w, h) = (r.width + 1, r.height + 1);
            let size = self.atlas.size;
            if self.atlas.cursor_x + w > size {
                self.atlas.cursor_x = 0;
                self.atlas.cursor_y += self.atlas.row_h;
                self.atlas.row_h = 0;
            }
            if self.atlas.cursor_y + h > size {
                // Still full: this and all colder glyphs stay evicted.
                continue;
            }
            let (x, y) = (self.atlas.cursor_x, self.atlas.cursor_y);
            self.atlas.cursor_x += w;
            self.atlas.row_h = self.atlas.row_h.max(h);
            self.queue.write_texture(
                wgpu::TexelCopyTextureInfo { texture: &self.atlas.texture, mip_level: 0, origin: wgpu::Origin3d { x, y, z: 0 }, aspect: wgpu::TextureAspect::All },
                &r.rgba,
                wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(r.width * 4), rows_per_image: Some(r.height) },
                wgpu::Extent3d { width: r.width, height: r.height, depth_or_array_layers: 1 },
            );
            self.atlas.map.insert(key, Some(AtlasEntry { x, y, w: r.width, h: r.height, left: r.left, top: r.top, is_color: r.is_color }));
            self.atlas.recency.insert(key, used);
        }
    }

    /// Draw a display list into `target` (`size_px` physical pixels, `scale` device px per logical px).
    /// Every GPU pass of the frame is recorded into one encoder and submitted exactly once,
    /// in display-list order. Nested layer renders share this encoder via [`Self::render_inner`].
    pub fn render(&mut self, list: &DisplayList, text: &mut TextSystem, target: &wgpu::TextureView, size_px: (u32, u32), scale: f32) {
        // Atlas recycling is only safe at the start of a TOP-LEVEL frame: nested
        // render() calls from render_layer share the outer frame's already-emitted
        // instances, and clearing/repacking the atlas mid-frame would leave those
        // instances pointing at stale glyph slots. Everything else (layer, instance
        // and batch counters) is per-frame bookkeeping that the top level owns too.
        if !self.rendering_layer {
            self.begin_frame(text);
            self.last_layers = 0;
            self.last_instances = 0;
            self.last_batches = 0;
            self.last_3d_meshes = 0;
            self.comp.slot_used = 0;
        }
        let mut enc = self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("rendrix frame") });
        self.render_inner(list, text, target, size_px, scale, &mut enc);
        self.queue.submit(Some(enc.finish()));
        self.draw_slots.append(&mut self.active_draw_slots);
        self.last_size = size_px;
    }

    /// Record a display list into `enc` without submitting. The ordinary no-layer path
    /// remains a single flat pass; layered frames interleave flat runs, snapshots, blurs
    /// and composites in the original paint order.
    fn render_inner(&mut self, list: &DisplayList, text: &mut TextSystem, target: &wgpu::TextureView, size_px: (u32, u32), scale: f32, enc: &mut wgpu::CommandEncoder) {
        if !list.has_layers {
            let mut slot = self.take_draw_slot();
            self.render_chunk(list, text, enc, target, size_px, scale, !self.rendering_layer, &mut slot);
            self.active_draw_slots.push(slot);
            return;
        }
        // Layer frames paint into an offscreen target we own. That lets a backdrop-filter
        // layer sample the already-painted prefix with a single GPU memcpy instead of
        // re-rasterising the whole prefix, so N backdrop layers cost O(N) work, not O(N^2).
        // The finished frame is published to the caller's view once, with a 1:1 blit.
        let segments = plan_segments(&list.items);
        let need_acc = segments.iter().any(|s| matches!(s, Segment::Layer { info, .. } if !info.backdrop.is_empty()));
        let frame = self.target(size_px.0, size_px.1);
        let acc = if need_acc { Some(self.target(size_px.0, size_px.1)) } else { None };
        // Fold the clear into the first flat pass when one exists; only a layers-only
        // frame (or a frame starting with a layer) needs a standalone clear pass.
        let first_is_flat = matches!(segments.first(), Some(Segment::Flat(_)));
        if !first_is_flat {
            self.clear_target(enc, &frame.view, list.background);
        }
        for (i, seg) in segments.into_iter().enumerate() {
            match seg {
                Segment::Flat(s) => {
                    let sub = DisplayList {
                        has_3d: s.items.iter().any(|i| matches!(i, Item::Scene3D(_))),
                        has_layers: false,
                        width: list.width,
                        height: list.height,
                        background: list.background,
                        items: s.items,
                    };
                    let mut slot = self.take_draw_slot();
                    self.render_chunk(&sub, text, enc, &frame.view, size_px, scale, i == 0 && first_is_flat, &mut slot);
                    self.active_draw_slots.push(slot);
                }
                Segment::Layer { info, open, children } => {
                    let backdrop = if !info.backdrop.is_empty() {
                        if let Some(a) = acc.as_ref() {
                            self.snapshot_frame(enc, &frame, a);
                            acc.as_ref()
                        } else {
                            None
                        }
                    } else {
                        None
                    };
                    self.render_layer(&info, &open, &children, list, text, &frame.view, backdrop, enc, size_px, scale);
                }
            }
        }
        self.blit_frame(enc, &frame, target, size_px);
        if let Some(a) = acc {
            self.release_target(a);
        }
        self.release_target(frame);
    }

    /// Draw one flat slice of a display list into the frame's shared encoder. The
    /// per-chunk `slot` owns the instance and 3D uniform buffers so later chunks cannot
    /// overwrite data still referenced by earlier passes in the same encoder.
    fn render_chunk(
        &mut self,
        list: &DisplayList,
        text: &mut TextSystem,
        enc: &mut wgpu::CommandEncoder,
        target: &wgpu::TextureView,
        size_px: (u32, u32),
        scale: f32,
        clear: bool,
        slot: &mut DrawSlot,
    ) {
        let mut insts: Vec<Inst> = Vec::with_capacity(512);
        let mut batches: Vec<Batch> = Vec::new();
        let mut scenes: Vec<SceneDraw> = Vec::new();
        self.flatten(list, text, scale, &mut insts, &mut batches, &mut scenes);
        // Accumulate across every chunk of the frame (flat runs, layer children and
        // backdrop re-renders). The top-level `render` resets these to zero.
        self.last_instances += insts.len();
        self.last_batches += batches.len();
        self.last_size = size_px;

        if insts.len() > slot.inst_cap {
            // Headroom growth: 25% + 256 instances of slack, rounded up, so a
            // frame that jitters around a power of two does not reallocate
            // every time it crosses the boundary.
            let want = insts.len() + insts.len() / 4 + 256;
            let mut cap = slot.inst_cap.max(1024);
            while cap < want {
                cap *= 2;
            }
            slot.inst_cap = cap;
            slot.inst_buf = self.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("rendrix instances"),
                size: (slot.inst_cap * std::mem::size_of::<Inst>()) as u64,
                usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            // The globals bind group references the instance buffer, so it must
            // be recreated — once per regrow, not once per frame.
            slot.globals_bind = Self::make_globals_bind(&self.device, &self.g_layout, &self.globals_buf, &slot.inst_buf);
        }
        if !insts.is_empty() {
            self.queue.write_buffer(&slot.inst_buf, 0, bytemuck::cast_slice(&insts));
        }
        self.queue.write_buffer(&slot.globals_buf, 0, bytemuck::bytes_of(&Globals { viewport: [size_px.0 as f32, size_px.1 as f32], pad: [0.0; 2] }));

        let bg = list.background;
        let load = if clear {
            wgpu::LoadOp::Clear(wgpu::Color { r: bg.r as f64, g: bg.g as f64, b: bg.b as f64, a: bg.a as f64 })
        } else {
            wgpu::LoadOp::Load
        };
        {
            let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("rendrix pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: target,
                    resolve_target: None,
                    ops: wgpu::Operations { load, store: wgpu::StoreOp::Store },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &slot.globals_bind, &[]);
            for b in &batches {
                let bind = match b.tex {
                    Tex::White => &self.white_bind,
                    Tex::Atlas => &self.atlas.bind,
                    Tex::ImageAtlas => &self.image_atlas.bind,
                    Tex::Image(id) => match self.images.get(&id) {
                        Some(i) => &i.bind,
                        None => continue,
                    },
                };
                pass.set_bind_group(1, bind, &[]);
                pass.draw(0..4, b.start..b.end);
            }
        }
        // 3D viewports composite on top of the 2D frame in the same encoder, so
        // `render_to_rgba` captures them too. Pure-2D frames skip everything: no
        // store lock (flag check), no depth texture, no extra submit.
        if list.has_3d && !scenes.is_empty() {
            let dv = self.depth_view(size_px.0.max(1), size_px.1.max(1));
            // `composite_3d` writes into `self.td_*` and may grow them. Swap in this
            // chunk's own buffers so a later chunk in the same frame cannot overwrite
            // uniforms referenced by this one's already-recorded passes.
            std::mem::swap(&mut self.td_frame_buf, &mut slot.td_frame_buf);
            std::mem::swap(&mut self.td_mesh_buf, &mut slot.td_mesh_buf);
            std::mem::swap(&mut self.td_bind, &mut slot.td_bind);
            self.last_3d_meshes += self.composite_3d(enc, target, &dv, &scenes, size_px);
            std::mem::swap(&mut self.td_frame_buf, &mut slot.td_frame_buf);
            std::mem::swap(&mut self.td_mesh_buf, &mut slot.td_mesh_buf);
            std::mem::swap(&mut self.td_bind, &mut slot.td_bind);
        } else {
            if !list.has_3d && !self.td_tris.is_empty() {
                self.td_tris.clear();
            }
        }
    }

    // ---- Offscreen layer implementation ---------------------------------------------

    /// Reuse an offscreen target of the requested size, creating one on demand.
    /// The idle pool is capped and evicts the least recently used target, so cycling
    /// through many distinct layer sizes cannot grow GPU memory without limit.
    fn target(&mut self, w: u32, h: u32) -> TargetTex {
        let (w, h) = (w.max(1), h.max(1));
        self.target_clock = self.target_clock.wrapping_add(1);
        let now = self.target_clock;
        if let Some(pos) = self.targets.iter().position(|t| t.width == w && t.height == h) {
            let mut t = self.targets.remove(pos);
            t.last_use = now;
            return t;
        }
        if self.targets.len() >= MAX_TARGETS {
            let lru = self.targets.iter().enumerate().min_by_key(|(_, t)| t.last_use).map(|(i, _)| i);
            if let Some(i) = lru {
                self.targets.remove(i);
            }
        }
        let uid = self.next_target_uid;
        self.next_target_uid = self.next_target_uid.wrapping_add(1);
        let texture = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("rendrix layer target"),
            size: wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: self.format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_SRC | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let view = texture.create_view(&Default::default());
        let bind = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("rendrix layer target bind"),
            layout: &self.tex_layout,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&view) },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::Sampler(&self.sampler) },
            ],
        });
        TargetTex { texture, view, bind, width: w, height: h, uid, last_use: now, comp_bind: None }
    }

    /// Return a target to the bounded LRU pool.
    fn release_target(&mut self, mut t: TargetTex) {
        t.last_use = self.target_clock;
        self.targets.push(t);
    }

    /// Number of idle offscreen targets currently retained (test-only accessor).
    #[cfg(test)]
    pub(crate) fn target_pool_len(&self) -> usize {
        self.targets.len()
    }

    /// Reserve a distinct uniform slot for one layer GPU pass. Slots are reused across
    /// frames; the cursor is reset only at the top-level frame so nested renders cannot
    /// overwrite a slot still referenced by the parent's unsubmitted encoder.
    fn take_comp_slot(&mut self) -> usize {
        let i = self.comp.slot_used;
        self.comp.slot_used += 1;
        if i == self.comp.slots.len() {
            let u_buf = self.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("rendrix composite slot uniforms"),
                size: std::mem::size_of::<LayerU>() as u64,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            let poly_buf = self.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("rendrix composite slot polygon"),
                size: (MAX_POLY_POINTS * 8) as u64,
                usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            let bind = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("rendrix composite slot bind"),
                layout: &self.comp.u_layout,
                entries: &[
                    wgpu::BindGroupEntry { binding: 0, resource: u_buf.as_entire_binding() },
                    wgpu::BindGroupEntry { binding: 1, resource: poly_buf.as_entire_binding() },
                ],
            });
            self.comp.slots.push(PassSlot { u_buf, poly_buf, bind });
        }
        i
    }

    /// Take an idle per-chunk draw slot (creating one on demand). Slots stay out of the
    /// pool for the whole frame so nested renders cannot reuse a buffer referenced by an
    /// unsubmitted pass.
    fn take_draw_slot(&mut self) -> DrawSlot {
        if let Some(slot) = self.draw_slots.pop() {
            return slot;
        }
        // Every slot needs its own allocation: `wgpu::Buffer::clone` shares the same GPU
        // memory, and two chunks writing into one buffer before the single submit would
        // make the last chunk's instances win for both passes.
        let inst_buf = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("rendrix draw slot instances"),
            size: (self.inst_cap * std::mem::size_of::<Inst>()) as u64,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let globals_buf = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("rendrix draw slot globals"),
            size: 16,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let globals_bind = Self::make_globals_bind(&self.device, &self.g_layout, &globals_buf, &inst_buf);
        let fs = self.td_frame_slot();
        let ms = self.td_mesh_slot();
        let td_frame_buf = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("rendrix draw slot 3d frame"),
            size: fs,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let td_mesh_buf = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("rendrix draw slot 3d mesh"),
            size: ms,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let td_bind = Self::make_td_bind(
            &self.device,
            &self.td_layout,
            &td_frame_buf,
            &td_mesh_buf,
            NonZeroU64::new(fs).unwrap(),
            NonZeroU64::new(ms).unwrap(),
        );
        DrawSlot { inst_buf, inst_cap: self.inst_cap, globals_buf, globals_bind, td_frame_buf, td_mesh_buf, td_bind }
    }

    /// Copy the finished offscreen frame into the sampleable backdrop accumulator.
    /// This replaces the old "re-render the entire prefix per backdrop layer" path:
    /// one GPU memcpy per backdrop layer instead of a full viewport rasterisation.
    fn snapshot_frame(&mut self, enc: &mut wgpu::CommandEncoder, frame: &TargetTex, acc: &TargetTex) {
        enc.copy_texture_to_texture(
            frame.texture.as_image_copy(),
            acc.texture.as_image_copy(),
            wgpu::Extent3d { width: frame.width.min(acc.width), height: frame.height.min(acc.height), depth_or_array_layers: 1 },
        );
    }

    /// Exact 1:1 blit of the finished offscreen frame into the caller's target view.
    /// The caller's view is write-only from this renderer's perspective, so a REPLACE
    /// textured triangle is the only safe way to publish the frame without needing the
    /// underlying `wgpu::Texture` handle for a texture-to-texture copy.
    fn blit_frame(&mut self, enc: &mut wgpu::CommandEncoder, frame: &TargetTex, target: &wgpu::TextureView, size_px: (u32, u32)) {
        let u = LayerU {
            viewport: [size_px.0 as f32, size_px.1 as f32, 0.0, 2.0], // mode = plain REPLACE
            layer_texel: [1.0 / frame.width as f32, 1.0 / frame.height as f32, 0.0, 0.0],
            backdrop_texel: [0.0; 4],
            rect: [0.0, 0.0, size_px.0 as f32, size_px.1 as f32],
            params: [1.0, 0.0, 0.0, 0.0],
            filter: [0.0, 1.0, 0.0, 0.0],
            fx2: [0.0; 4],
        };
        self.queue.write_buffer(&self.comp.u_buf, 0, bytemuck::bytes_of(&u));
        let tb = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("rendrix blit bind"),
            layout: &self.comp.t_layout,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&frame.view) },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(&self.white_view) },
                wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::TextureView(&self.white_view) },
                wgpu::BindGroupEntry { binding: 3, resource: wgpu::BindingResource::Sampler(&self.comp.sampler) },
            ],
        });
        {
            let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("rendrix blit pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: target,
                    resolve_target: None,
                    ops: wgpu::Operations { load: wgpu::LoadOp::Load, store: wgpu::StoreOp::Store },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });
            pass.set_pipeline(&self.comp.blit_pipeline);
            pass.set_bind_group(0, &self.comp.u_bind, &[]);
            pass.set_bind_group(1, &tb, &[]);
            pass.draw(0..3, 0..1);
        }
    }

    /// Clear one offscreen target to `bg`, recorded into the shared frame encoder.
    fn clear_target(&mut self, enc: &mut wgpu::CommandEncoder, view: &wgpu::TextureView, bg: Color) {
        {
            let _pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("rendrix layer clear pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color { r: bg.r as f64, g: bg.g as f64, b: bg.b as f64, a: bg.a as f64 }),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });
        }
    }

    /// Horizontal Gaussian pass into `dst` from one of the source textures, recorded into
    /// the frame's shared encoder under its own uniform slot.
    fn blur_horizontal(&mut self, enc: &mut wgpu::CommandEncoder, dst: &TargetTex, sigma: f32, source_is_backdrop: bool, layer_tex: &wgpu::TextureView, backdrop_tex: &wgpu::TextureView) {
        let u = LayerU {
            viewport: [dst.width as f32, dst.height as f32, sigma, if source_is_backdrop { 1.0 } else { 0.0 }],
            layer_texel: [1.0 / dst.width as f32, 1.0 / dst.height as f32, 0.0, 0.0],
            backdrop_texel: [0.0; 4],
            rect: [0.0, 0.0, dst.width as f32, dst.height as f32],
            params: [0.0; 4],
            filter: [0.0; 4],
            fx2: [0.0; 4],
        };
        let slot = self.take_comp_slot();
        self.queue.write_buffer(&self.comp.slots[slot].u_buf, 0, bytemuck::bytes_of(&u));
        let ub = self.comp.slots[slot].bind.clone();
        let tb = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("rendrix blur-h bind"),
            layout: &self.comp.t_layout,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(layer_tex) },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(backdrop_tex) },
                wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::TextureView(backdrop_tex) },
                wgpu::BindGroupEntry { binding: 3, resource: wgpu::BindingResource::Sampler(&self.comp.sampler) },
            ],
        });
        {
            let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("rendrix blur-h pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &dst.view,
                    resolve_target: None,
                    ops: wgpu::Operations { load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT), store: wgpu::StoreOp::Store },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });
            pass.set_pipeline(&self.comp.blur_pipeline);
            pass.set_bind_group(0, &ub, &[]);
            pass.set_bind_group(1, &tb, &[]);
            pass.draw(0..4, 0..1);
        }
    }

    /// Render one isolated layer: children into a transparent target, then composite it
    /// (optionally over the already-captured `backdrop`) into the offscreen frame.
    fn render_layer(
        &mut self,
        info: &LayerInfo,
        open: &[Item],
        children: &[Item],
        list: &DisplayList,
        text: &mut TextSystem,
        target: &wgpu::TextureView,
        backdrop: Option<&TargetTex>,
        enc: &mut wgpu::CommandEncoder,
        size_px: (u32, u32),
        scale: f32,
    ) {
        // Ancestor transforms replayed around the layer give its device-space rect.
        let mut xf = [scale, 0.0, 0.0, scale, 0.0, 0.0];
        for item in open {
            if let Item::PushTransform(m) = item {
                xf = mat_mul(xf, *m);
            }
        }
        let r = info.rect;
        let corners = [
            mat_apply(xf, r[0], r[1]),
            mat_apply(xf, r[0] + r[2], r[1]),
            mat_apply(xf, r[0], r[1] + r[3]),
            mat_apply(xf, r[0] + r[2], r[1] + r[3]),
        ];
        let (x0f, x1f) = corners.iter().fold((f32::MAX, f32::MIN), |a, c| (a.0.min(c.0), a.1.max(c.0)));
        let (y0f, y1f) = corners.iter().fold((f32::MAX, f32::MIN), |a, c| (a.0.min(c.1), a.1.max(c.1)));
        let blur = |ops: &[FilterOp]| ops.iter().find_map(|op| if let FilterOp::Blur(r) = op { Some(*r) } else { None }).unwrap_or(0.0);
        let (cb, bb) = (blur(&info.filter), blur(&info.backdrop));
        // The separable kernel reaches +/-3.5 sigma in each direction; pad enough that
        // the blurred halo is not clipped by the offscreen target bounds.
        let reach = (cb + bb) * 3.5 * scale + 2.0;
        let x0 = (x0f - reach).floor().max(0.0);
        let y0 = (y0f - reach).floor().max(0.0);
        let x1 = (x1f + reach).ceil().min(size_px.0 as f32);
        let y1 = (y1f + reach).ceil().min(size_px.1 as f32);
        let (lw, lh) = ((x1 - x0) as u32, (y1 - y0) as u32);
        if lw == 0 || lh == 0 {
            return;
        }
        self.last_layers += 1;

        // Children, shifted so the layer's device-space origin lands at the target origin.
        let mut child_items = vec![Item::PushTransform([1.0, 0.0, 0.0, 1.0, -x0, -y0])];
        child_items.extend_from_slice(open);
        child_items.extend_from_slice(children);
        for item in open.iter().rev() {
            if let Some(c) = closing(item) {
                child_items.push(c);
            }
        }
        child_items.push(Item::PopTransform);
        let child_list = DisplayList {
            width: list.width,
            height: list.height,
            background: Color::from_u8(0, 0, 0, 0.0),
            items: child_items,
            has_3d: children.iter().any(|i| matches!(i, Item::Scene3D(_))),
            has_layers: children.iter().any(|i| matches!(i, Item::PushLayer(_))),
        };
        let mut layer_tex = self.target(lw, lh);
        // Pooled targets keep their previous contents. The layer quad covers the padded
        // rect (reach + 2px), which `coverage()` treats as fully inside, so any stale
        // pixel in that ring would be composited. Always start transparent.
        self.clear_target(enc, &layer_tex.view, Color::from_u8(0, 0, 0, 0.0));
        let prev_rendering_layer = self.rendering_layer;
        self.rendering_layer = true;
        self.render_inner(&child_list, text, &layer_tex.view, (lw, lh), scale, enc);
        self.rendering_layer = prev_rendering_layer;

        // The raw backdrop is the accumulated prefix snapshot (never the texture being
        // written to), so the REPLACE composite can cross-fade antialiased edges with it.
        let raw_view = backdrop.map(|b| b.view.clone()).unwrap_or_else(|| self.white_view.clone());
        let backdrop_needed = !info.backdrop.is_empty() && backdrop.is_some();
        let (csig, bsig) = (cb * scale, bb * scale);
        // Separable blur: horizontal half into a layer-sized / full-viewport intermediate,
        // vertical half in the composite fragment shader. No-blur layers skip both passes.
        let content_h = if csig > 0.05 {
            Some(self.target(lw, lh))
        } else {
            None
        };
        if let Some(h) = content_h.as_ref() {
            self.blur_horizontal(enc, h, csig, false, &layer_tex.view, &raw_view);
        }
        let backdrop_h = if backdrop_needed && bsig > 0.05 {
            Some(self.target(size_px.0, size_px.1))
        } else {
            None
        };
        if let Some(h) = backdrop_h.as_ref() {
            self.blur_horizontal(enc, h, bsig, true, &layer_tex.view, &raw_view);
        }
        let layer_view = content_h.as_ref().map(|t| t.view.clone()).unwrap_or_else(|| layer_tex.view.clone());
        let filtered_view = backdrop_h.as_ref().map(|t| t.view.clone()).unwrap_or_else(|| raw_view.clone());
        let sig = [
            content_h.as_ref().map(|t| t.uid).unwrap_or(layer_tex.uid),
            backdrop_h.as_ref().map(|t| t.uid).unwrap_or_else(|| backdrop.map(|b| b.uid).unwrap_or(0)),
            backdrop.map(|b| b.uid).unwrap_or(0),
        ];

        // Filter factors fold exactly for grayscale/brightness; blur is split between
        // the horizontal pass above and the vertical taps in the composite shader.
        let (gray, bright, _) = filter_fold(std::slice::from_ref(&info.filter));
        // `backdrop-filter` grayscale/brightness are separate from the layer's own filter.
        let (bg_gray, bg_bright, _) = filter_fold(std::slice::from_ref(&info.backdrop));
        let mut poly_px = Vec::with_capacity(info.polygon.len());
        for p in &info.polygon {
            let (px, py) = mat_apply(xf, r[0] + p[0], r[1] + p[1]);
            poly_px.push([px - x0, py - y0]);
        }
        let u = LayerU {
            // viewport.w = 1 selects REPLACE blending with the backdrop already resolved.
            viewport: [size_px.0 as f32, size_px.1 as f32, 0.0, if backdrop_needed { 1.0 } else { 0.0 }],
            layer_texel: [1.0 / lw as f32, 1.0 / lh as f32, 0.0, 0.0],
            backdrop_texel: [1.0 / size_px.0 as f32, 1.0 / size_px.1 as f32, 0.0, 0.0],
            rect: [x0, y0, lw as f32, lh as f32],
            params: [info.opacity, csig, bsig, info.radius * scale],
            filter: [gray, bright, if backdrop_needed { 1.0 } else { 0.0 }, poly_px.len().min(MAX_POLY_POINTS) as f32],
            fx2: [bg_gray, bg_bright, 0.0, 0.0],
        };
        let slot = self.take_comp_slot();
        self.queue.write_buffer(&self.comp.slots[slot].u_buf, 0, bytemuck::bytes_of(&u));
        if !poly_px.is_empty() {
            let mut data = vec![[0.0f32, 0.0f32]; MAX_POLY_POINTS];
            for (d, p) in data.iter_mut().zip(poly_px.iter().take(MAX_POLY_POINTS)) {
                *d = *p;
            }
            self.queue.write_buffer(&self.comp.slots[slot].poly_buf, 0, bytemuck::cast_slice(&data));
        }
        let ub = self.comp.slots[slot].bind.clone();
        if layer_tex.comp_bind.as_ref().is_none_or(|(s, _)| *s != sig) {
            let bind = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("rendrix composite textures bind"),
                layout: &self.comp.t_layout,
                entries: &[
                    wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&layer_view) },
                    wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(&filtered_view) },
                    wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::TextureView(&raw_view) },
                    wgpu::BindGroupEntry { binding: 3, resource: wgpu::BindingResource::Sampler(&self.comp.sampler) },
                ],
            });
            layer_tex.comp_bind = Some((sig, bind));
        }
        let tbind = layer_tex.comp_bind.as_ref().unwrap().1.clone();
        let pipeline = if backdrop_needed { &self.comp.replace_pipeline } else { &self.comp.pipeline };
        {
            let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("rendrix composite pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: target,
                    resolve_target: None,
                    ops: wgpu::Operations { load: wgpu::LoadOp::Load, store: wgpu::StoreOp::Store },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });
            pass.set_pipeline(pipeline);
            pass.set_bind_group(0, &ub, &[]);
            pass.set_bind_group(1, &tbind, &[]);
            pass.draw(0..4, 0..1);
        }
        if let Some(h) = content_h {
            self.release_target(h);
        }
        if let Some(h) = backdrop_h {
            self.release_target(h);
        }
        self.release_target(layer_tex);
    }

    /// Render to an offscreen RGBA8 image and read it back (used for screenshots and tests).
    pub fn render_to_rgba(&mut self, list: &DisplayList, text: &mut TextSystem, size_px: (u32, u32), scale: f32) -> Vec<u8> {
        let (w, h) = size_px;
        // A zero extent is not a valid texture, and the shader divides by the
        // viewport width/height — short-circuit before any GPU work.
        if w == 0 || h == 0 {
            return Vec::new();
        }
        // Defensive clamp (also keeps every row-pitch calculation valid).
        let (w, h) = (w.max(1), h.max(1));
        let tex = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("rendrix capture"),
            size: wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: self.format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let view = tex.create_view(&Default::default());
        self.render(list, text, &view, size_px, scale);
        let padded = (w * 4).div_ceil(256) * 256;
        let buf = self.device.create_buffer(&wgpu::BufferDescriptor { label: Some("rendrix readback"), size: (padded * h) as u64, usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ, mapped_at_creation: false });
        let mut enc = self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("rendrix readback") });
        enc.copy_texture_to_buffer(
            tex.as_image_copy(),
            wgpu::TexelCopyBufferInfo { buffer: &buf, layout: wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(padded), rows_per_image: Some(h) } },
            wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
        );
        self.queue.submit(Some(enc.finish()));
        let slice = buf.slice(..);
        let (tx, rx) = std::sync::mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |r| {
            let _ = tx.send(r);
        });
        let _ = self.device.poll(wgpu::PollType::Wait);
        rx.recv().expect("map channel").expect("map readback");
        let data = slice.get_mapped_range();
        let mut out = Vec::with_capacity((w * h * 4) as usize);
        for row in 0..h {
            let s = (row * padded) as usize;
            out.extend_from_slice(&data[s..s + (w * 4) as usize]);
        }
        drop(data);
        buf.unmap();
        if matches!(self.format, wgpu::TextureFormat::Bgra8Unorm | wgpu::TextureFormat::Bgra8UnormSrgb) {
            for px in out.chunks_exact_mut(4) {
                px.swap(0, 2);
            }
        }
        out
    }

    // ------------------------------------------------------------------------------------

    fn flatten(&mut self, list: &DisplayList, text: &mut TextSystem, scale: f32, insts: &mut Vec<Inst>, batches: &mut Vec<Batch>, scenes: &mut Vec<SceneDraw>) {
        let mut st = State {
            xf: vec![[scale, 0.0, 0.0, scale, 0.0, 0.0]],
            clip: vec![ClipEntry { rect: NO_CLIP, radius: 0.0, ellipse: false }],
            opacity: vec![1.0],
            filter: Vec::new(),
            backdrop: Vec::new(),
        };

        for item in &list.items {
            let xf = *st.xf.last().unwrap();
            let top = *st.clip.last().unwrap();
            let (clip, clip_r, clip_e) = (top.rect, top.radius, top.ellipse);
            let op = *st.opacity.last().unwrap();
            // Fold the filter stack to per-instance factors (pointwise ops are exact;
            // blur is expanded into offset copies at emit time).
            let (f_gray, f_bright, f_blur) = filter_fold(&st.filter);
            let det_sc = (xf[0] * xf[3] - xf[1] * xf[2]).abs().sqrt().max(1e-3);
            let blur_local = if f_blur > 0.0 { f_blur * 0.5 / det_sc } else { 0.0 };
            let clip_kind = if clip_e { 1.0 } else { 0.0 };
            let base = |rect: [f32; 4], kind: f32| Inst {
                rect,
                xf0: [xf[0], xf[1], xf[2], xf[3]],
                xf1: [xf[4], xf[5], kind, 0.0],
                clip,
                // Shapes sample the atlas's white block (see `Renderer::new`).
                uv: [1.0 / ATLAS_SIZE as f32, 1.0 / ATLAS_SIZE as f32, 0.0, 0.0],
                extra: [0.0, 0.0, clip_r, 0.0],
                clip_info: [clip_kind, f_gray, f_bright, 0.0],
                ..Default::default()
            };
            match item {
                // Layers are split out before flattening; if one slips through (e.g. from a
                // caller-built list) treat the markers as transparent to draw state.
                Item::PushLayer(_) | Item::PopLayer => {}
                Item::Rect(r) => {
                    let mut i = base(r.rect, 0.0);
                    i.radii = clamp_radii(r.radii, r.rect[2], r.rect[3]);
                    match &r.fill {
                        Fill::None => {}
                        Fill::Solid(c) => i.c[0] = mul_alpha(*c, op),
                        Fill::Linear { angle, stops } => set_gradient(&mut i, 1.0, *angle, stops, op),
                        Fill::Radial { stops } => set_gradient(&mut i, 2.0, 0.0, stops, op),
                    }
                    if let Some(b) = &r.border {
                        i.border = b.widths;
                        // Keep the legacy single-colour field in sync for inset-shadow reuse
                        // and for the unlayered fallback path.
                        i.bcolor = mul_alpha(b.colors[0], op);
                        for k in 0..4 {
                            i.bcolors[k] = mul_alpha(b.colors[k], op);
                            i.bstyle[k] = b.styles[k] as f32;
                        }
                    }
                    if matches!(r.fill, Fill::None) && r.border.is_none() {
                        continue;
                    }
                    push_inst(insts, batches, Tex::Atlas, i, blur_local);
                }
                Item::Shadow(s) => {
                    let mut i = base(s.rect, if s.inset { 2.0 } else { 1.0 });
                    i.radii = clamp_radii(s.radii, s.rect[2], s.rect[3]);
                    i.c[0] = mul_alpha(s.color, op);
                    i.extra[0] = s.blur;
                    i.extra[3] = if s.inset { 0.0 } else { s.blur };
                    if s.inset {
                        i.border = s.hole;
                        i.bcolor = clamp_radii(s.radii, s.hole[2], s.hole[3]);
                    }
                    push_inst(insts, batches, Tex::Atlas, i, blur_local);
                }
                Item::Image(im) => {
                    let mut i = base(im.rect, 4.0);
                    i.radii = clamp_radii(im.radii, im.rect[2], im.rect[3]);
                    i.c[0] = [1.0, 1.0, 1.0, op];
                    // Small images live in the shared atlas: remap their normalised
                    // sub-rect into the slot's patch of that texture so consecutive
                    // images share one bind group and merge into one batch. Canvas
                    // ids and large/atlas-full images keep the dedicated texture and
                    // the original UVs — shapes, text and canvases are untouched.
                    if let Some(&(sx, sy, sw, sh)) = self.image_atlas.slots.get(&im.image) {
                        let a = self.image_atlas.size as f32;
                        i.uv = [
                            (sx as f32 + 1.0 + im.uv[0] * sw as f32) / a,
                            (sy as f32 + 1.0 + im.uv[1] * sh as f32) / a,
                            im.uv[2] * sw as f32 / a,
                            im.uv[3] * sh as f32 / a,
                        ];
                        push_inst(insts, batches, Tex::ImageAtlas, i, blur_local);
                    } else {
                        i.uv = im.uv;
                        push_inst(insts, batches, Tex::Image(im.image), i, blur_local);
                    }
                }
                Item::Text(t) => {
                    let simple = xf[1].abs() < 1e-6 && xf[2].abs() < 1e-6;
                    if simple {
                        // Lazy-shape offscreen paragraphs: skip shaping, atlas
                        // uploads and instances for text fully outside the clip.
                        let (dx0, dy0) = mat_apply(xf, t.x, t.y);
                        if !text_maybe_visible(dx0, dy0, t.width * xf[0].abs(), clip) {
                            continue;
                        }
                    }
                    let para = text.paragraph(&t.content, Some(t.width));
                    let sc = (xf[0] * xf[3] - xf[1] * xf[2]).abs().sqrt().max(1e-3);
                    for g in &para.glyphs {
                        let run = &t.content.runs[g.run.min(t.content.runs.len() - 1)];
                        let color = mul_alpha(run.style.color, op);
                        if color[3] <= 0.0 {
                            continue;
                        }
                        let (ox, oy) = (t.x, t.y + g.line_y);
                        let phys = g.glyph.physical((0.0, 0.0), sc);
                        let Some(e) = self.atlas_entry(text, phys.cache_key) else { continue };
                        // UVs are fractions of the actual atlas edge (the test
                        // renderer uses a tiny atlas; production uses 2048).
                        let s = self.atlas.size as f32;
                        let uv = [e.x as f32 / s, e.y as f32 / s, e.w as f32 / s, e.h as f32 / s];
                        let kind = if e.is_color { 4.0 } else { 3.0 };
                        let mut i;
                        if simple {
                            // Pixel-snapped placement in device space.
                            // `phys` already carries the glyph's own x/y offset within the line.
                            let (dx, dy) = mat_apply(xf, ox, oy);
                            let px = (dx.round() as i32 + phys.x + e.left) as f32;
                            let py = (dy.round() as i32 + phys.y - e.top) as f32;
                            // Fully-clipped glyphs need no atlas slot and no instance.
                            if px + e.w as f32 <= clip[0]
                                || px >= clip[0] + clip[2]
                                || py + e.h as f32 <= clip[1]
                                || py >= clip[1] + clip[3]
                            {
                                continue;
                            }
                            i = Inst { rect: [px, py, e.w as f32, e.h as f32], xf0: [1.0, 0.0, 0.0, 1.0], xf1: [0.0, 0.0, kind, 0.0], clip, extra: [0.0, 0.0, clip_r, 0.0], clip_info: [clip_kind, f_gray, f_bright, 0.0], ..Default::default() };
                        } else {
                            // Rotated/skewed text: place the glyph quad in local space and let the matrix transform it.
                            let lx = ox + (phys.x + e.left) as f32 / sc;
                            let ly = oy + (phys.y - e.top) as f32 / sc;
                            i = base([lx, ly, e.w as f32 / sc, e.h as f32 / sc], kind);
                        }
                        i.c[0] = color;
                        i.uv = uv;
                        push_inst(insts, batches, Tex::Atlas, i, blur_local);
                    }
                    // Underline / line-through spans.
                    for seg in &para.segments {
                        let run = &t.content.runs[seg.run.min(t.content.runs.len() - 1)];
                        let deco = run.style.text_decoration;
                        if !(deco.underline || deco.line_through || deco.overline) {
                            continue;
                        }
                        let thick = (run.style.font_size / 14.0).max(1.0);
                        let mut lines = Vec::new();
                        if deco.underline {
                            lines.push(seg.line_y + run.style.font_size * 0.12);
                        }
                        if deco.line_through {
                            lines.push(seg.line_y - run.style.font_size * 0.3);
                        }
                        if deco.overline {
                            lines.push(seg.line_y - run.style.font_size * 0.85);
                        }
                        for y in lines {
                            let mut i = base([t.x + seg.x, t.y + y, seg.w, thick], 0.0);
                            i.c[0] = mul_alpha(run.style.color, op);
                            push_inst(insts, batches, Tex::Atlas, i, blur_local);
                        }
                    }
                }
                Item::PushClip { rect, radius } => {
                    let r = intersect_rect(xf, rect, clip);
                    let sc = (xf[0] * xf[3] - xf[1] * xf[2]).abs().sqrt();
                    st.clip.push(ClipEntry { rect: r, radius: radius * sc, ellipse: false });
                }
                Item::PushEllipseClip { rect } => {
                    let r = intersect_rect(xf, rect, clip);
                    st.clip.push(ClipEntry { rect: r, radius: 0.0, ellipse: true });
                }
                Item::PopClip => {
                    if st.clip.len() > 1 {
                        st.clip.pop();
                    }
                }
                Item::PushTransform(m) => st.xf.push(mat_mul(xf, *m)),
                Item::PopTransform => {
                    if st.xf.len() > 1 {
                        st.xf.pop();
                    }
                }
                Item::PushOpacity(o) => st.opacity.push(op * o),
                Item::PopOpacity => {
                    if st.opacity.len() > 1 {
                        st.opacity.pop();
                    }
                }
                Item::PushFilter(ops) => st.filter.push(ops.clone()),
                Item::PopFilter => {
                    st.filter.pop();
                }
                Item::PushBackdrop(ops) => st.backdrop.push(ops.clone()),
                Item::PopBackdrop => {
                    st.backdrop.pop();
                }
                // 3D viewports ignore opacity (opaque forward pass in v1) but honour
                // the transform + clip stacks: the rect is resolved to device pixels
                // here and the GPU pass re-applies it as viewport + scissor.
                Item::Scene3D(s) => {
                    if s.rect[2] <= 0.0 || s.rect[3] <= 0.0 {
                        continue;
                    }
                    let corners = [mat_apply(xf, s.rect[0], s.rect[1]), mat_apply(xf, s.rect[0] + s.rect[2], s.rect[1]), mat_apply(xf, s.rect[0], s.rect[1] + s.rect[3]), mat_apply(xf, s.rect[0] + s.rect[2], s.rect[1] + s.rect[3])];
                    let (x0, x1) = corners.iter().fold((f32::MAX, f32::MIN), |a, c| (a.0.min(c.0), a.1.max(c.0)));
                    let (y0, y1) = corners.iter().fold((f32::MAX, f32::MIN), |a, c| (a.0.min(c.1), a.1.max(c.1)));
                    let (ix0, iy0) = (x0.max(clip[0]), y0.max(clip[1]));
                    let (ix1, iy1) = (x1.min(clip[0] + clip[2]), y1.min(clip[1] + clip[3]));
                    if ix1 - ix0 >= 0.5 && iy1 - iy0 >= 0.5 {
                        scenes.push(SceneDraw { scene_id: s.scene_id, rect_px: [ix0, iy0, ix1 - ix0, iy1 - iy0] });
                    }
                }
            }
        }
        let _ = IDENTITY;
    }
}

impl Renderer {
    /// Composite 3D scene viewports over an already-rendered 2D frame.
    ///
    /// `draws` carry physical-pixel rects (see [`SceneDraw`]); each resolves its
    /// scene from the [`crate::scene3d`] store, sets viewport + scissor to the CSS
    /// layout rect, and draws depth-tested. Returns the number of meshes drawn.
    /// Draws with missing scenes, empty meshes or fully-culled content cost no pass.
    fn composite_3d(
        &mut self,
        enc: &mut wgpu::CommandEncoder,
        target: &wgpu::TextureView,
        depth_view: &wgpu::TextureView,
        draws: &[SceneDraw],
        size_px: (u32, u32),
    ) -> usize {
        // Phase 1 (needs `&mut`): upload any uncached raw-triangle meshes.
        for d in draws {
            let Some(scene) = cloned_scene(d.scene_id) else { continue };
            for (mi, mesh) in scene.meshes.iter().enumerate() {
                let Geometry::Tris { verts, indices } = &mesh.geometry else { continue };
                let key = (d.scene_id, mi, scene.revision);
                if self.td_tris.contains_key(&key) {
                    continue;
                }
                let v: Vec<V3> = verts.iter().copied().map(V3::from).collect();
                let vbuf = self.device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                    label: Some("rendrix 3d tris"),
                    contents: bytemuck::cast_slice(&v),
                    usage: wgpu::BufferUsages::VERTEX,
                });
                let ibuf = self.device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                    label: Some("rendrix 3d tris idx"),
                    contents: bytemuck::cast_slice(indices),
                    usage: wgpu::BufferUsages::INDEX,
                });
                self.td_tris.insert(key, GpuMesh { vbuf, ibuf, count: indices.len() as u32 });
            }
        }
        // Phase 2a (read-only scene state): prepare every FrameU and MeshU into its
        // own slot. All passes share one encoder and one submit, so queue writes
        // from inside a draw loop would all land before the submit and the last
        // scene/mesh would win for every draw. Writing distinct slots up front
        // avoids that entirely.
        #[derive(Clone, Copy)]
        enum TdGeom {
            Box,
            Plane,
            Sphere,
            Tris((u64, usize, u64)),
        }
        struct TdMeshSlot {
            slot: usize,
            geom: TdGeom,
        }
        struct TdPreparedDraw {
            frame_slot: usize,
            vp: [f32; 4],
            scissor: [u32; 4],
            meshes: Vec<TdMeshSlot>,
        }

        let mut frames: Vec<FrameU> = Vec::new();
        let mut meshes: Vec<MeshU> = Vec::new();
        let mut prepared: Vec<TdPreparedDraw> = Vec::new();
        let mut used: Vec<(u64, usize, u64)> = Vec::new();
        for d in draws {
            let (rx, ry, rw, rh) = (d.rect_px[0], d.rect_px[1], d.rect_px[2], d.rect_px[3]);
            if rw < 1.0 || rh < 1.0 {
                continue;
            }
            // `cloned_scene` is a cheap Arc clone; deref it, never deep-copy the Scene.
            let Some(scene) = cloned_scene(d.scene_id) else { continue };
            if scene.meshes.is_empty() {
                continue;
            }
            let vp = scene.camera.view_proj(rw, rh);
            let visible = scene.visible_meshes(&vp);
            if visible.is_empty() {
                continue;
            }
            // Clamp the viewport into the target; fully-offscreen draws are skipped.
            let (tx, ty) = (size_px.0 as f32, size_px.1 as f32);
            let (cx0, cy0) = (rx.max(0.0), ry.max(0.0));
            let (cx1, cy1) = ((rx + rw).min(tx), (ry + rh).min(ty));
            if cx1 - cx0 < 1.0 || cy1 - cy0 < 1.0 {
                continue;
            }
            let (dir, lcol) = scene.key_light();
            let amb = scene.ambient();
            let frame = FrameU {
                view_proj: vp.to_cols_array_2d(),
                cam_pos: [scene.camera.position.x, scene.camera.position.y, scene.camera.position.z, 1.0],
                light_dir: [dir.x, dir.y, dir.z, 0.0],
                light_color: [lcol[0], lcol[1], lcol[2], 1.0],
                ambient: [amb[0], amb[1], amb[2], 1.0],
            };
            let mut mesh_slots = Vec::with_capacity(visible.len());
            for mi in visible {
                let mesh = &scene.meshes[mi];
                if mesh.world_sphere().1 < 1e-9 {
                    continue;
                }
                let nmat = mesh.transform.inverse().transpose();
                let uni = MeshU {
                    model: mesh.transform.to_cols_array_2d(),
                    normal_mat: nmat.to_cols_array_2d(),
                    base: mesh.material.base_color,
                    emissive: [mesh.material.emissive[0], mesh.material.emissive[1], mesh.material.emissive[2], 1.0],
                    params: [mesh.material.metallic, mesh.material.roughness, 128.0 + (8.0 - 128.0) * mesh.material.roughness, 0.0],
                };
                let slot = meshes.len();
                meshes.push(uni);
                let geom = match &mesh.geometry {
                    Geometry::Box => TdGeom::Box,
                    Geometry::Plane => TdGeom::Plane,
                    Geometry::Sphere => TdGeom::Sphere,
                    Geometry::Tris { .. } => {
                        let key = (d.scene_id, mi, scene.revision);
                        used.push(key);
                        TdGeom::Tris(key)
                    }
                };
                mesh_slots.push(TdMeshSlot { slot, geom });
            }
            if mesh_slots.is_empty() {
                continue;
            }
            let frame_slot = frames.len();
            frames.push(frame);
            prepared.push(TdPreparedDraw {
                frame_slot,
                vp: [cx0, cy0, cx1 - cx0, cy1 - cy0],
                scissor: [cx0 as u32, cy0 as u32, (cx1 - cx0) as u32, (cy1 - cy0) as u32],
                meshes: mesh_slots,
            });
        }

        // Drop GPU buffers for scenes/meshes that no longer exist (bounded cache).
        if used.len() != self.td_tris.len() {
            self.td_tris.retain(|k, _| used.contains(k));
        }
        // Zero draws / empty scenes: no buffer writes, no passes, no GPU work.
        if prepared.is_empty() {
            return 0;
        }

        // Phase 2b: grow once if needed, then write each uniform into its own slot
        // before recording any pass. Offsets are slot_index * aligned_slot_size.
        self.ensure_td_capacity(frames.len(), meshes.len());
        let (fs, ms) = (self.td_frame_slot(), self.td_mesh_slot());
        for (i, f) in frames.iter().enumerate() {
            self.queue.write_buffer(&self.td_frame_buf, i as u64 * fs, bytemuck::bytes_of(f));
        }
        for (i, m) in meshes.iter().enumerate() {
            self.queue.write_buffer(&self.td_mesh_buf, i as u64 * ms, bytemuck::bytes_of(m));
        }

        // Phase 2c: one render pass per visible scene (viewport/scissor/depth logic
        // unchanged), selecting the per-scene and per-mesh slot for every draw.
        let mut drawn = 0usize;
        for p in prepared {
            let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("rendrix 3d pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: target,
                    resolve_target: None,
                    ops: wgpu::Operations { load: wgpu::LoadOp::Load, store: wgpu::StoreOp::Store },
                })],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: depth_view,
                    depth_ops: Some(wgpu::Operations { load: wgpu::LoadOp::Clear(1.0), store: wgpu::StoreOp::Discard }),
                    stencil_ops: None,
                }),
                timestamp_writes: None,
                occlusion_query_set: None,
            });
            pass.set_pipeline(&self.td_pipeline);
            pass.set_viewport(p.vp[0], p.vp[1], p.vp[2], p.vp[3], 0.0, 1.0);
            pass.set_scissor_rect(p.scissor[0], p.scissor[1], p.scissor[2], p.scissor[3]);
            let frame_off = (p.frame_slot as u64) * fs;
            for pm in p.meshes {
                let gpu: &GpuMesh = match pm.geom {
                    TdGeom::Box => &self.td_box,
                    TdGeom::Plane => &self.td_plane,
                    TdGeom::Sphere => &self.td_sphere,
                    TdGeom::Tris(key) => match self.td_tris.get(&key) {
                        Some(g) => g,
                        None => continue,
                    },
                };
                let mesh_off = (pm.slot as u64) * ms;
                pass.set_bind_group(0, &self.td_bind, &[frame_off as u32, mesh_off as u32]);
                pass.set_vertex_buffer(0, gpu.vbuf.slice(..));
                pass.set_index_buffer(gpu.ibuf.slice(..), wgpu::IndexFormat::Uint32);
                pass.draw_indexed(0..gpu.count, 0, 0..1);
                drawn += 1;
            }
        }
        drawn
    }

    /// Depth texture for the 3D pass, recreated only when the viewport size changes.
    fn depth_view(&mut self, w: u32, h: u32) -> wgpu::TextureView {
        let rebuild = self.depth.as_ref().is_none_or(|(dw, dh, _)| *dw != w || *dh != h);
        if rebuild {
            let tex = self.device.create_texture(&wgpu::TextureDescriptor {
                label: Some("rendrix 3d depth"),
                size: wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Depth24Plus,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
                view_formats: &[],
            });
            self.depth = Some((w, h, tex));
        }
        self.depth.as_ref().unwrap().2.create_view(&Default::default())
    }

    /// Composite caller-supplied 3D viewports over `output_view` (standalone entry
    /// point; `render` calls the same path internally for display-list scenes).
    /// `depth_view` must match `output_view`'s size. Zero draws = zero GPU work.
    pub fn render_3d_scenes(&mut self, draws: &[SceneDraw], output_view: &wgpu::TextureView, depth_view: &wgpu::TextureView) -> usize {
        if draws.is_empty() {
            return 0;
        }
        let size = self.last_size;
        let mut enc = self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("rendrix 3d frame") });
        let n = self.composite_3d(&mut enc, output_view, depth_view, draws, size);
        if n > 0 {
            self.queue.submit(Some(enc.finish()));
        }
        self.last_3d_meshes = n;
        n
    }
}

fn is_open(item: &Item) -> bool {
    matches!(item, Item::PushTransform(_) | Item::PushClip { .. } | Item::PushEllipseClip { .. } | Item::PushOpacity(_) | Item::PushFilter(_) | Item::PushBackdrop(_))
}

fn is_close(item: &Item) -> bool {
    matches!(item, Item::PopTransform | Item::PopClip | Item::PopOpacity | Item::PopFilter | Item::PopBackdrop)
}

fn closing(item: &Item) -> Option<Item> {
    match item {
        Item::PushTransform(_) => Some(Item::PopTransform),
        Item::PushClip { .. } | Item::PushEllipseClip { .. } => Some(Item::PopClip),
        Item::PushOpacity(_) => Some(Item::PopOpacity),
        Item::PushFilter(_) => Some(Item::PopFilter),
        Item::PushBackdrop(_) => Some(Item::PopBackdrop),
        _ => None,
    }
}

/// Index of the `PopLayer` matching the `PushLayer` at `start`.
fn matching_pop(items: &[Item], start: usize) -> usize {
    let mut depth = 1usize;
    for (i, item) in items.iter().enumerate().skip(start) {
        match item {
            Item::PushLayer(_) => depth += 1,
            Item::PopLayer => {
                depth -= 1;
                if depth == 0 {
                    return i;
                }
            }
            _ => {}
        }
    }
    items.len().saturating_sub(1)
}

/// Split a display list into flat runs and isolated layers. Ancestor transform/clip
/// markers open at a boundary are replayed around each run and each layer's children so
/// flatten state (and hit-test geometry) stays identical to the un-split frame.
fn plan_segments(items: &[Item]) -> Vec<Segment> {
    let mut out = Vec::new();
    let mut open: Vec<Item> = Vec::new();
    let mut chunk: Vec<Item> = Vec::new();
    let mut chunk_base = 0usize;
    let mut i = 0usize;
    while i < items.len() {
        match &items[i] {
            Item::PushLayer(info) => {
                if !chunk.is_empty() {
                    let mut flat = open[..chunk_base].to_vec();
                    flat.append(&mut chunk);
                    for item in open[..chunk_base].iter().rev() {
                        if let Some(c) = closing(item) {
                            flat.push(c);
                        }
                    }
                    out.push(Segment::Flat(FlatSegment { items: flat }));
                }
                let end = matching_pop(items, i + 1);
                out.push(Segment::Layer {
                    info: info.clone(),
                    open: open.clone(),
                    children: if end > i + 1 { items[i + 1..end].to_vec() } else { Vec::new() },
                });
                i = (end + 1).min(items.len());
                chunk_base = open.len();
            }
            Item::PopLayer => {
                i += 1;
            }
            item if is_open(item) => {
                open.push(item.clone());
                chunk.push(item.clone());
                i += 1;
            }
            item if is_close(item) => {
                open.pop();
                // If this closed an ancestor that was open when the chunk started, the
                // replay prefix shrinks with it.
                if chunk_base > open.len() {
                    chunk_base = open.len();
                }
                chunk.push(item.clone());
                i += 1;
            }
            item => {
                chunk.push(item.clone());
                i += 1;
            }
        }
    }
    if !chunk.is_empty() {
        let mut flat = open[..chunk_base].to_vec();
        flat.append(&mut chunk);
        for item in open[..chunk_base].iter().rev() {
            if let Some(c) = closing(item) {
                flat.push(c);
            }
        }
        out.push(Segment::Flat(FlatSegment { items: flat }));
    }
    out
}

fn mul_alpha(c: Color, k: f32) -> [f32; 4] {
    [c.r, c.g, c.b, c.a * k]
}

fn clamp_radii(r: [f32; 4], w: f32, h: f32) -> [f32; 4] {
    // CSS: scale all radii down uniformly if adjacent radii exceed a side.
    let f = [
        if r[0] + r[1] > 0.0 { w / (r[0] + r[1]) } else { f32::MAX },
        if r[1] + r[2] > 0.0 { h / (r[1] + r[2]) } else { f32::MAX },
        if r[2] + r[3] > 0.0 { w / (r[2] + r[3]) } else { f32::MAX },
        if r[3] + r[0] > 0.0 { h / (r[3] + r[0]) } else { f32::MAX },
    ]
    .into_iter()
    .fold(1.0f32, f32::min);
    [r[0] * f, r[1] * f, r[2] * f, r[3] * f]
}

fn set_gradient(i: &mut Inst, kind: f32, angle: f32, stops: &[(Color, f32)], op: f32) {
    let n = stops.len().min(4);
    i.xf1[3] = kind;
    i.extra[0] = angle;
    i.extra[1] = n as f32;
    for (k, (c, p)) in stops.iter().take(4).enumerate() {
        i.c[k] = mul_alpha(*c, op);
        i.stops[k] = *p;
    }
    // Stops beyond the 4th are dropped; make the last kept stop the end of the ramp.
    if stops.len() > 4 {
        i.stops[3] = 1.0;
    }
}

/// Fold a filter stack to per-instance factors: `(grayscale 0..1, brightness, blur radius px)`.
/// Sequential grayscale ops compose as `1 - Π(1 - g)`; brightness multiplies; blur radii add
/// (capped — the offset-copy expansion below is O(blur) work per instance).
fn filter_fold(stack: &[Vec<FilterOp>]) -> (f32, f32, f32) {
    let (mut g, mut b, mut r) = (0.0f32, 1.0f32, 0.0f32);
    for ops in stack {
        for op in ops {
            match op {
                FilterOp::Grayscale(x) => g += (1.0 - g) * x,
                FilterOp::Brightness(x) => b *= x,
                FilterOp::Blur(x) => r += x,
            }
        }
    }
    (g.clamp(0.0, 1.0), b.max(0.0), r.clamp(0.0, 24.0))
}

/// Intersect a clip rect (local space) with the current clip, both in device space.
fn intersect_rect(xf: Matrix, rect: &[f32; 4], clip: [f32; 4]) -> [f32; 4] {
    let corners = [mat_apply(xf, rect[0], rect[1]), mat_apply(xf, rect[0] + rect[2], rect[1]), mat_apply(xf, rect[0], rect[1] + rect[3]), mat_apply(xf, rect[0] + rect[2], rect[1] + rect[3])];
    let (x0, x1) = corners.iter().fold((f32::MAX, f32::MIN), |a, c| (a.0.min(c.0), a.1.max(c.0)));
    let (y0, y1) = corners.iter().fold((f32::MAX, f32::MIN), |a, c| (a.0.min(c.1), a.1.max(c.1)));
    let (nx0, ny0, nx1, ny1) = (x0.max(clip[0]), y0.max(clip[1]), x1.min(clip[0] + clip[2]), y1.min(clip[1] + clip[3]));
    [nx0, ny0, (nx1 - nx0).max(0.0), (ny1 - ny0).max(0.0)]
}

/// Emit one instance, merging into the current batch when the texture matches.
/// `blur` is the local-space offset step for `filter: blur()` (0 = one copy): the content is
/// expanded into a centre + 4 axis copies whose weights sum to 1, a box-blur approximation
/// that keeps batching identical (same texture, consecutive instances).
fn push_inst(insts: &mut Vec<Inst>, batches: &mut Vec<Batch>, tex: Tex, inst: Inst, blur: f32) {
    let emit = |insts: &mut Vec<Inst>, batches: &mut Vec<Batch>, inst: Inst| {
        let idx = insts.len() as u32;
        insts.push(inst);
        match batches.last_mut() {
            Some(b) if b.tex == tex && b.end == idx => b.end += 1,
            _ => batches.push(Batch { tex, start: idx, end: idx + 1 }),
        }
    };
    if blur <= 0.0 {
        emit(insts, batches, inst);
        return;
    }
    // Copies: the centre paints at full strength (opaque interiors stay exact) and the
    // four axis offsets add a soft halo. Instance colors are non-premultiplied, so only
    // the alpha is scaled — scaling rgb too would double-premultiply in the shader.
    const COPIES: [(f32, f32, f32); 5] = [(0.0, 0.0, 1.0), (1.0, 0.0, 0.25), (-1.0, 0.0, 0.25), (0.0, 1.0, 0.25), (0.0, -1.0, 0.25)];
    for (dx, dy, w) in COPIES {
        let mut c = inst.clone();
        c.rect[0] += dx * blur;
        c.rect[1] += dy * blur;
        for k in 0..4 {
            c.c[k][3] *= w;
        }
        emit(insts, batches, c);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::GpuContext;
    use crate::display::Border;
    use rendrix_css::Color;

    fn gfx_border_solid(widths: [f32; 4], color: Color) -> Border {
        Border::solid(widths, color)
    }

    fn render(list: &DisplayList, w: u32, h: u32) -> Option<Vec<u8>> {
        let ctx = GpuContext::headless().ok()?;
        let mut r = Renderer::new(ctx.device, ctx.queue, wgpu::TextureFormat::Rgba8Unorm);
        let mut t = TextSystem::new();
        Some(r.render_to_rgba(list, &mut t, (w, h), 1.0))
    }

    fn px(img: &[u8], w: u32, x: u32, y: u32) -> [u8; 4] {
        let i = ((y * w + x) * 4) as usize;
        [img[i], img[i + 1], img[i + 2], img[i + 3]]
    }

    fn rect(x: f32, y: f32, w: f32, h: f32, c: Color) -> Item {
        Item::Rect(RectItem { rect: [x, y, w, h], radii: [0.0; 4], fill: Fill::Solid(c), border: None })
    }

    #[test]
    fn draws_filled_rects_in_order() {
        let mut l = DisplayList::new(64.0, 64.0, Color::WHITE);
        l.push(rect(8.0, 8.0, 32.0, 32.0, Color::from_u8(255, 0, 0, 1.0)));
        l.push(rect(24.0, 24.0, 32.0, 32.0, Color::from_u8(0, 0, 255, 1.0)));
        let Some(img) = render(&l, 64, 64) else { eprintln!("no GPU adapter, skipping");  return };
        assert_eq!(px(&img, 64, 2, 2), [255, 255, 255, 255]);
        assert_eq!(px(&img, 64, 12, 12), [255, 0, 0, 255]);
        assert_eq!(px(&img, 64, 30, 30), [0, 0, 255, 255], "later item paints over earlier");
        assert_eq!(px(&img, 64, 50, 50), [0, 0, 255, 255]);
    }

    #[test]
    fn per_side_border_colors_and_dashes() {
        let mut l = DisplayList::new(64.0, 64.0, Color::WHITE);
        let sides = Border {
            widths: [6.0; 4],
            colors: [
                Color::from_u8(255, 0, 0, 1.0),   // top
                Color::from_u8(0, 255, 0, 1.0),   // right
                Color::from_u8(0, 0, 255, 1.0),   // bottom
                Color::from_u8(255, 255, 0, 1.0), // left
            ],
            styles: [0; 4],
        };
        l.push(Item::Rect(RectItem {
            rect: [8.0, 8.0, 48.0, 48.0],
            radii: [0.0; 4],
            fill: Fill::Solid(Color::BLACK),
            border: Some(sides),
        }));
        let Some(img) = render(&l, 64, 64) else { return };
        assert_eq!(px(&img, 64, 32, 10), [255, 0, 0, 255], "top border is red");
        assert_eq!(px(&img, 64, 54, 32), [0, 255, 0, 255], "right border is green");
        assert_eq!(px(&img, 64, 32, 54), [0, 0, 255, 255], "bottom border is blue");
        assert_eq!(px(&img, 64, 10, 32), [255, 255, 0, 255], "left border is yellow");

        // Dashed border alternates colour and fill along the edge.
        let mut d = DisplayList::new(64.0, 64.0, Color::WHITE);
        d.push(Item::Rect(RectItem {
            rect: [8.0, 8.0, 48.0, 48.0],
            radii: [0.0; 4],
            fill: Fill::Solid(Color::WHITE),
            border: Some(Border {
                widths: [6.0; 4],
                colors: [Color::from_u8(255, 0, 0, 1.0); 4],
                styles: [1; 4],
            }),
        }));
        let Some(img) = render(&d, 64, 64) else { return };
        assert_eq!(px(&img, 64, 10, 10), [255, 0, 0, 255], "dash is painted");
        assert_eq!(px(&img, 64, 28, 10), [255, 255, 255, 255], "gap shows the fill");
    }

    #[test]
    fn rounded_corners_borders_and_clip() {
        let mut l = DisplayList::new(64.0, 64.0, Color::WHITE);
        l.push(Item::Rect(RectItem { rect: [4.0, 4.0, 56.0, 56.0], radii: [20.0; 4], fill: Fill::Solid(Color::BLACK), border: Some(gfx_border_solid([4.0; 4], Color::from_u8(0, 255, 0, 1.0))) }));
        l.push(Item::PushClip { rect: [0.0, 0.0, 64.0, 10.0], radius: 0.0 });
        l.push(rect(0.0, 0.0, 64.0, 64.0, Color::from_u8(255, 0, 255, 0.5)));
        l.push(Item::PopClip);
        let Some(img) = render(&l, 64, 64) else { return };
        assert_eq!(px(&img, 64, 1, 30), [255, 255, 255, 255], "outside the box");
        let corner = px(&img, 64, 5, 33);
        assert!(corner[1] > 200 && corner[0] < 60, "left border is green: {corner:?}");
        assert_eq!(px(&img, 64, 1, 11), [255, 255, 255, 255], "rounded corner shows background");
        let center = px(&img, 64, 32, 32);
        assert_eq!(center, [0, 0, 0, 255], "fill inside the border; clip excluded the magenta wash");
        let clipped = px(&img, 64, 1, 5);
        assert!(clipped[0] > 240 && clipped[2] > 240 && (clipped[1] as i32 - 128).abs() < 4, "inside clip: translucent magenta over white: {clipped:?}");
        assert_eq!(px(&img, 64, 1, 20), [255, 255, 255, 255], "below the clip nothing is washed");
    }

    #[test]
    fn linear_gradient_runs_left_to_right() {
        let mut l = DisplayList::new(64.0, 8.0, Color::WHITE);
        l.push(Item::Rect(RectItem {
            rect: [0.0, 0.0, 64.0, 8.0],
            radii: [0.0; 4],
            fill: Fill::Linear { angle: std::f32::consts::FRAC_PI_2, stops: vec![(Color::BLACK, 0.0), (Color::WHITE, 1.0)] },
            border: None,
        }));
        let Some(img) = render(&l, 64, 8) else { return };
        let (a, b, c) = (px(&img, 64, 2, 4)[0], px(&img, 64, 32, 4)[0], px(&img, 64, 61, 4)[0]);
        assert!(a < 30 && (b as i32 - 128).abs() < 12 && c > 225, "{a} {b} {c}");
    }

    #[test]
    fn transform_rotates_geometry() {
        let mut l = DisplayList::new(100.0, 100.0, Color::WHITE);
        l.push(Item::PushTransform([0.0, 1.0, -1.0, 0.0, 100.0, 0.0])); // rotate 90° about origin, shifted right
        l.push(rect(0.0, 0.0, 40.0, 10.0, Color::BLACK)); // becomes a 10-wide, 40-tall column at x=90..100
        l.push(Item::PopTransform);
        let Some(img) = render(&l, 100, 100) else { return };
        assert_eq!(px(&img, 100, 95, 30), [0, 0, 0, 255]);
        assert_eq!(px(&img, 100, 30, 5), [255, 255, 255, 255]);
    }

    #[test]
    fn text_ink_matches_the_measured_extent() {
        use rendrix_css::ComputedStyle;
        use rendrix_layout::{InlineContent, InlineRun, TextMeasurer};
        use std::sync::Arc;
        let mut s = ComputedStyle::default();
        s.font_size = 32.0;
        s.color = Color::BLACK;
        let s = Arc::new(s);
        let content = Arc::new(InlineContent::new(vec![InlineRun { text: "Hello, Rendrix".into(), style: s.clone(), node: 1, is_break: false }], s));
        let mut ts = TextSystem::new();
        let (mw, mh) = ts.measure(&content, None);
        let (x0, y0) = (10.0f32, 8.0f32);
        let (w, h) = ((mw + 40.0) as u32, (mh + 24.0) as u32);
        let mut l = DisplayList::new(w as f32, h as f32, Color::WHITE);
        l.push(Item::Text(TextItem { content, x: x0, y: y0, width: mw }));
        let Some(ctx) = GpuContext::headless().ok() else { return };
        let mut r = Renderer::new(ctx.device, ctx.queue, wgpu::TextureFormat::Rgba8Unorm);
        let img = r.render_to_rgba(&l, &mut ts, (w, h), 1.0);
        let mut ink: Vec<(u32, u32)> = Vec::new();
        for y in 0..h {
            for x in 0..w {
                if px(&img, w, x, y)[0] < 128 {
                    ink.push((x, y));
                }
            }
        }
        assert!(ink.len() > 80, "text produced ink: {}", ink.len());
        let (minx, maxx) = (ink.iter().map(|p| p.0).min().unwrap() as f32, ink.iter().map(|p| p.0).max().unwrap() as f32);
        let (miny, maxy) = (ink.iter().map(|p| p.1).min().unwrap() as f32, ink.iter().map(|p| p.1).max().unwrap() as f32);
        assert!(minx >= x0 - 1.0 && minx <= x0 + 6.0, "ink starts at the text origin: {minx}");
        assert!(maxx <= x0 + mw + 1.0 && maxx >= x0 + mw * 0.85, "ink ends at the measured width ({maxx} vs {}), i.e. glyphs are not double-spaced", x0 + mw);
        assert!(miny >= y0 - 1.0 && maxy <= y0 + mh + 1.0, "ink stays inside the measured line box: {miny}..{maxy} vs {y0}..{}", y0 + mh);
    }

    #[test]
    fn rects_batch_into_one_draw() {
        let mut l = DisplayList::new(200.0, 200.0, Color::WHITE);
        for i in 0..50 {
            l.push(rect(i as f32 * 3.0, 10.0, 2.0, 2.0, Color::BLACK));
        }
        // Plus a translucent rect: the premultiplied-alpha contract must hold.
        l.push(Item::Rect(RectItem { rect: [10.0, 100.0, 40.0, 40.0], radii: [0.0; 4], fill: Fill::Solid(Color::from_u8(255, 0, 0, 0.5)), border: None }));
        let Some(ctx) = GpuContext::headless().ok() else { return };
        let mut r = Renderer::new(ctx.device, ctx.queue, wgpu::TextureFormat::Rgba8Unorm);
        let mut t = TextSystem::new();
        let img = r.render_to_rgba(&l, &mut t, (200, 200), 1.0);
        assert_eq!(r.last_instances, 51, "one instance per rect");
        assert_eq!(r.last_batches, 1, "atlas-sampled rects merge into a single batch, one pipeline");
        let c = px(&img, 200, 30, 120);
        assert!(c[0] > 240 && (c[1] as i32 - 127).abs() < 16 && (c[2] as i32 - 127).abs() < 16 && c[3] == 255, "50% red over white is pink-opaque: {c:?}");
    }

    #[test]
    fn atlas_overflow_recovers_without_stale_frames() {
        use read_fonts::{FontRef, TableProvider};
        use rendrix_css::ComputedStyle;
        use rendrix_layout::{InlineContent, InlineRun};
        use std::sync::Arc;
        /// Distinct codepoints covered by the bundled Inter Regular, in cmap order.
        /// The old version used CJK codepoints resolved through per-OS fallback
        /// fonts; advances/rasters differ per platform, so atlas packing (and the
        /// exact post-recycle instance count) differed on macOS vs Windows.
        /// Inter ships in-repo, so metrics — and packing — are identical everywhere.
        fn inter_codepoints(n: usize) -> Vec<char> {
            let bytes = include_bytes!("../fonts/Inter_400Regular.ttf");
            let font = FontRef::new(&bytes[..]).expect("bundled Inter parses");
            let cmap = font.cmap().expect("bundled Inter has a cmap");
            let (_, _, sub) = cmap.best_subtable().expect("Inter has a Unicode subtable");
            let cps: Vec<char> = sub.iter().filter_map(|(cp, _)| char::from_u32(cp)).take(n).collect();
            assert_eq!(cps.len(), n, "bundled Inter covers {n} distinct codepoints");
            cps
        }
        let Some(ctx) = GpuContext::headless().ok() else { return };
        // A tiny atlas forces evictions with a modest glyph set. Every glyph
        // comes from Inter, so each needs its own atlas slot on every OS.
        let cps = inter_codepoints(500);
        let text: String = cps.iter().collect();
        let mut r = Renderer::with_atlas_size(ctx.device, ctx.queue, wgpu::TextureFormat::Rgba8Unorm, 256);
        let mut t = TextSystem::new();
        t.load_font(include_bytes!("../fonts/Inter_400Regular.ttf").to_vec());
        let mut s = ComputedStyle::default();
        s.font_size = 32.0;
        s.font_family = Arc::from(vec!["Inter".to_string()]);
        s.color = Color::BLACK;
        let s = Arc::new(s);
        let content = Arc::new(InlineContent::new(vec![InlineRun { text, style: s.clone(), node: 1, is_break: false }], s));
        let mut l = DisplayList::new(2000.0, 2000.0, Color::WHITE);
        l.push(Item::Text(TextItem { content, x: 8.0, y: 8.0, width: 1984.0 }));
        let img1 = r.render_to_rgba(&l, &mut t, (320, 200), 1.0);
        assert_eq!(r.atlas_generation(), 0, "overflow never clears mid-frame");
        let (inst1, batch1) = (r.last_instances, r.last_batches);
        assert!(inst1 > 30, "head glyphs still drawn: {inst1}");
        // The next frame recycles the atlas at frame start, then uploads the
        // same glyphs in the same order into the same slots: pixel-identical,
        // with no stale placements anywhere.
        let img2 = r.render_to_rgba(&l, &mut t, (320, 200), 1.0);
        assert!(r.atlas_evictions() >= 1, "the overflowed atlas was recycled");
        assert_eq!((r.last_instances, r.last_batches), (inst1, batch1));
        assert_eq!(img1, img2, "deferred clear is pixel-stable across frames");
        let img3 = r.render_to_rgba(&l, &mut t, (320, 200), 1.0);
        assert_eq!(img2, img3, "steady state stays stable");
        // … and real ink landed in the top-left where the text starts.
        assert!(img1.chunks_exact(4).any(|p| p[0] < 128), "text produced ink");
        // LRU retention: the hot head glyphs survived the recycle, so drawing
        // only them faults nothing new in.
        assert!(r.atlas_generation() >= 1, "recycle bumped the generation");
        let head: String = cps[..20].iter().collect();
        let mut hs = ComputedStyle::default();
        hs.font_size = 32.0;
        hs.font_family = Arc::from(vec!["Inter".to_string()]);
        hs.color = Color::BLACK;
        let hs = Arc::new(hs);
        let head_content =
            Arc::new(InlineContent::new(vec![InlineRun { text: head, style: hs.clone(), node: 1, is_break: false }], hs));
        let mut hl = DisplayList::new(2000.0, 2000.0, Color::WHITE);
        hl.push(Item::Text(TextItem { content: head_content, x: 8.0, y: 8.0, width: 1984.0 }));
        // First head-only frame recycles the previous frame's leftover overflow
        // flag; every glyph is already resident so nothing faults in.
        r.render_to_rgba(&hl, &mut t, (320, 200), 1.0);
        let ev = r.atlas_evictions();
        // Second head-only frame: steady hot state, no overflow, no recycle.
        r.render_to_rgba(&hl, &mut t, (320, 200), 1.0);
        assert_eq!(r.atlas_evictions(), ev, "hot glyphs retained, no new recycle");
    }

    #[test]
    fn box_shadow_is_soft_and_outside() {
        let mut l = DisplayList::new(80.0, 80.0, Color::WHITE);
        l.push(Item::Shadow(ShadowItem { rect: [20.0, 20.0, 40.0, 40.0], radii: [4.0; 4], blur: 16.0, color: Color::BLACK, inset: false, hole: [0.0; 4] }));
        let Some(img) = render(&l, 80, 80) else { return };
        let inside = px(&img, 80, 40, 40)[0];
        let near = px(&img, 80, 62, 40)[0];
        let far = px(&img, 80, 78, 40)[0];
        assert!(inside < 40, "dense under the box: {inside}");
        assert!(near > inside && near < 255 && far > near, "falls off with distance: {inside} {near} {far}");
    }

    // ---- Shared image atlas ----------------------------------------------------------------

    #[test]
    fn small_images_share_one_atlas_batch() {
        let Some(ctx) = GpuContext::headless().ok() else { eprintln!("no GPU adapter, skipping"); return };
        let mut r = Renderer::new(ctx.device, ctx.queue, wgpu::TextureFormat::Rgba8Unorm);
        let mut red = Vec::new();
        let mut blue = Vec::new();
        for _ in 0..16 {
            red.extend_from_slice(&[255, 0, 0, 255]);
            blue.extend_from_slice(&[0, 0, 255, 255]);
        }
        let red_id = r.upload_image(&red, 4, 4);
        let blue_id = r.upload_image(&blue, 4, 4);
        assert!(r.image_atlas.slots.contains_key(&red_id), "small images go to the shared atlas");
        assert!(r.image_atlas.slots.contains_key(&blue_id));
        assert!(r.images.is_empty(), "no per-image textures are created for small images");
        let mut l = DisplayList::new(64.0, 32.0, Color::WHITE);
        l.push(Item::Image(ImageItem { image: red_id, rect: [0.0, 0.0, 32.0, 32.0], radii: [0.0; 4], uv: [0.0, 0.0, 1.0, 1.0] }));
        l.push(Item::Image(ImageItem { image: blue_id, rect: [32.0, 0.0, 32.0, 32.0], radii: [0.0; 4], uv: [0.0, 0.0, 1.0, 1.0] }));
        let mut t = TextSystem::new();
        let img = r.render_to_rgba(&l, &mut t, (64, 32), 1.0);
        assert_eq!(r.last_batches, 1, "two adjacent atlas images merge into one batch (old code: 2)");
        assert_eq!(r.last_instances, 2);
        assert_eq!(px(&img, 64, 16, 16), [255, 0, 0, 255]);
        assert_eq!(px(&img, 64, 48, 16), [0, 0, 255, 255]);
    }

    #[test]
    fn image_reupload_overwrites_and_large_images_fall_back() {
        let Some(ctx) = GpuContext::headless().ok() else { eprintln!("no GPU adapter, skipping"); return };
        let mut r = Renderer::new(ctx.device, ctx.queue, wgpu::TextureFormat::Rgba8Unorm);
        let solid = |rgba: [u8; 4]| {
            let mut v = Vec::with_capacity(16 * 4);
            for _ in 0..16 { v.extend_from_slice(&rgba); }
            v
        };
        let id = r.upload_image(&solid([255, 0, 0, 255]), 4, 4);
        let slot = *r.image_atlas.slots.get(&id).unwrap();
        r.upload_image_as(id, &solid([0, 0, 255, 255]), 4, 4);
        assert_eq!(*r.image_atlas.slots.get(&id).unwrap(), slot, "same-size re-upload reuses the slot");
        let mut l = DisplayList::new(16.0, 16.0, Color::WHITE);
        l.push(Item::Image(ImageItem { image: id, rect: [0.0, 0.0, 16.0, 16.0], radii: [0.0; 4], uv: [0.0, 0.0, 1.0, 1.0] }));
        let mut t = TextSystem::new();
        let img = r.render_to_rgba(&l, &mut t, (16, 16), 1.0);
        assert_eq!(px(&img, 16, 8, 8), [0, 0, 255, 255], "re-upload overwrote stale pixels");
        // Oversized images keep the dedicated per-image texture path.
        let big = r.upload_image(&vec![255u8; 513 * 4], 513, 1);
        assert!(!r.image_atlas.slots.contains_key(&big));
        assert!(r.images.contains_key(&big));
        assert_eq!(r.image_size(big), Some((513, 1)));
    }

    // ---- Canvas 2D targets (A3) ------------------------------------------------------------

    fn solid_canvas(w: u32, h: u32, c: Color) -> Vec<u8> {
        let (r, g, b, a) = ((c.r * 255.0) as u8, (c.g * 255.0) as u8, (c.b * 255.0) as u8, (c.a * 255.0) as u8);
        let mut v = Vec::with_capacity((w * h * 4) as usize);
        for _ in 0..w * h {
            v.extend_from_slice(&[r, g, b, a]);
        }
        v
    }

    #[test]
    fn canvas_composites_over_dom_background() {
        let Some(ctx) = GpuContext::headless().ok() else { return };
        let mut r = Renderer::new(ctx.device, ctx.queue, wgpu::TextureFormat::Rgba8Unorm);
        let mut t = TextSystem::new();
        // DOM background: solid green rect covering the canvas area.
        let mut l = DisplayList::new(64.0, 64.0, Color::WHITE);
        l.push(rect(4.0, 4.0, 24.0, 24.0, Color::from_u8(0, 255, 0, 1.0)));
        // Canvas bitmap: opaque red 24x24, composited on top like paint.rs emits it.
        let node = 41u32;
        r.upload_canvas(node, &solid_canvas(24, 24, Color::from_u8(255, 0, 0, 1.0)), 24, 24);
        l.push(Item::Image(ImageItem { image: crate::display::canvas_image_id(node), rect: [4.0, 4.0, 24.0, 24.0], radii: [0.0; 4], uv: [0.0, 0.0, 1.0, 1.0] }));
        let img = r.render_to_rgba(&l, &mut t, (64, 64), 1.0);
        assert_eq!(px(&img, 64, 16, 16), [255, 0, 0, 255], "canvas paints over the DOM background");
        assert_eq!(px(&img, 64, 2, 2), [255, 255, 255, 255]);
        assert_eq!(px(&img, 64, 40, 40), [255, 255, 255, 255]);
        assert_eq!(r.last_batches, 2, "one batch for DOM quads, one for the canvas texture");
    }

    #[test]
    fn canvas_resize_recreates_the_target() {
        let Some(ctx) = GpuContext::headless().ok() else { return };
        let mut r = Renderer::new(ctx.device, ctx.queue, wgpu::TextureFormat::Rgba8Unorm);
        let node = 7u32;
        r.upload_canvas(node, &solid_canvas(4, 4, Color::BLACK), 4, 4);
        assert_eq!(r.canvas_size(node), Some((4, 4)));
        // Same size: texture reused, contents refreshed.
        r.upload_canvas(node, &solid_canvas(4, 4, Color::WHITE), 4, 4);
        assert_eq!(r.canvas_size(node), Some((4, 4)));
        // New size: target recreated.
        r.upload_canvas(node, &solid_canvas(8, 2, Color::WHITE), 8, 2);
        assert_eq!(r.canvas_size(node), Some((8, 2)));
        // Dirty-region upload keeps the size and refreshes pixels.
        r.update_canvas_region(node, &solid_canvas(8, 2, Color::BLACK), 8, 2, [0, 0, 8, 2]);
        assert_eq!(r.canvas_size(node), Some((8, 2)));
        // A canvas id never collides with decoded-image ids (which count up from 1).
        assert!(crate::display::canvas_image_id(node) >= 0x8000_0000);
        r.remove_canvas(node);
        assert_eq!(r.canvas_size(node), None);
    }

    #[test]
    fn canvas_zero_size_owns_no_texture() {
        let Some(ctx) = GpuContext::headless().ok() else { return };
        let mut r = Renderer::new(ctx.device, ctx.queue, wgpu::TextureFormat::Rgba8Unorm);
        r.upload_canvas(9, &[], 0, 0);
        assert_eq!(r.canvas_size(9), None);
    }

    // ---- 3D scene compositing (R6.2 v1) ------------------------------------------------

    #[test]
    fn scene3d_box_composites_over_the_2d_frame() {
        use crate::scene3d;
        let Some(ctx) = GpuContext::headless().ok() else { return };
        let mut r = Renderer::new(ctx.device, ctx.queue, wgpu::TextureFormat::Rgba8Unorm);
        let mut t = TextSystem::new();
        let id = 0x3D00_0001u64;
        scene3d::upsert_scene(
            id,
            r#"{"camera": {"position": [0, 0.6, 4.2], "lookAt": [0, 0, 0]},
                "meshes": [{"geo": {"kind": "box"},
                            "material": {"color": [1, 0, 0, 1]},
                            "transform": {"scale": [2, 2, 2]}}]}"#,
        )
        .unwrap();
        // Pure-2D control frame first: no scenes anywhere, zero 3D cost.
        let mut plain = DisplayList::new(64.0, 64.0, Color::WHITE);
        plain.push(rect(0.0, 0.0, 64.0, 64.0, Color::WHITE));
        let img0 = r.render_to_rgba(&plain, &mut t, (64, 64), 1.0);
        assert_eq!(r.last_3d_meshes, 0, "no Scene3D items: nothing drawn");
        assert!(img0.chunks_exact(4).all(|p| p == [255, 255, 255, 255]));
        // Same frame with a scene viewport: the red box must land on top.
        let mut l = DisplayList::new(64.0, 64.0, Color::WHITE);
        l.push(Item::Scene3D(Scene3DItem { scene_id: id, rect: [4.0, 4.0, 56.0, 56.0] }));
        let img = r.render_to_rgba(&l, &mut t, (64, 64), 1.0);
        scene3d::remove_scene(id);
        assert_eq!(r.last_3d_meshes, 1, "one visible box mesh drawn");
        let red = img.chunks_exact(4).filter(|p| p[0] > 150 && p[1] < 120 && p[2] < 120).count();
        assert!(red > 100, "box painted {red} reddish pixels");
        assert_eq!(px(&img, 64, 1, 1), [255, 255, 255, 255], "outside the viewport stays background");
        // A missing scene paints nothing (transparent, never wrong).
        let mut l = DisplayList::new(64.0, 64.0, Color::WHITE);
        l.push(Item::Scene3D(Scene3DItem { scene_id: 0x3D00_9999, rect: [4.0, 4.0, 56.0, 56.0] }));
        let img = r.render_to_rgba(&l, &mut t, (64, 64), 1.0);
        assert_eq!(r.last_3d_meshes, 0);
        assert!(img.chunks_exact(4).all(|p| p == [255, 255, 255, 255]));
    }

    /// Regression: all draws share one encoder/submit, so uniform writes must use
    /// distinct dynamic-offset slots. With a single slot, both boxes below render
    /// with the *last* mesh's transform+material (two overlapping blue boxes).
    #[test]
    fn scene3d_two_meshes_keep_their_own_transform_and_color() {
        use crate::scene3d;
        let Some(ctx) = GpuContext::headless().ok() else { return };
        let mut r = Renderer::new(ctx.device, ctx.queue, wgpu::TextureFormat::Rgba8Unorm);
        let mut t = TextSystem::new();
        let id = 0x3D00_0002u64;
        scene3d::upsert_scene(
            id,
            r#"{"camera": {"position": [0, 0, 5], "lookAt": [0, 0, 0]},
                "meshes": [
                  {"geo": {"kind": "box"},
                   "material": {"color": [1, 0, 0, 1]},
                   "transform": {"pos": [-1.8, 0, 0], "scale": [1.6, 1.6, 1.6]}},
                  {"geo": {"kind": "box"},
                   "material": {"color": [0, 0, 1, 1]},
                   "transform": {"pos": [1.8, 0, 0], "scale": [1.6, 1.6, 1.6]}}
                ]}"#,
        )
        .unwrap();
        let mut l = DisplayList::new(64.0, 64.0, Color::WHITE);
        l.push(Item::Scene3D(Scene3DItem { scene_id: id, rect: [0.0, 0.0, 64.0, 64.0] }));
        let img = r.render_to_rgba(&l, &mut t, (64, 64), 1.0);
        scene3d::remove_scene(id);
        assert_eq!(r.last_3d_meshes, 2, "both box meshes drawn");

        let is_red = |p: &[u8]| p[0] > 150 && p[1] < 120 && p[2] < 120;
        let is_blue = |p: &[u8]| p[2] > 150 && p[0] < 120 && p[1] < 120;
        let (mut red_count, mut blue_count) = (0u32, 0u32);
        let (mut red_max_x, mut blue_min_x) = (0u32, 64u32);
        for y in 0..64u32 {
            for x in 0..64u32 {
                let p = px(&img, 64, x, y);
                if is_red(&p) {
                    red_count += 1;
                    red_max_x = red_max_x.max(x);
                }
                if is_blue(&p) {
                    blue_count += 1;
                    blue_min_x = blue_min_x.min(x);
                }
            }
        }
        assert!(red_count > 100, "red mesh painted {red_count} pixels");
        assert!(blue_count > 100, "blue mesh painted {blue_count} pixels");
        assert!(red_max_x < blue_min_x, "distinct spots: red ends at x={red_max_x}, blue starts at x={blue_min_x}");
    }

    #[test]
    fn render_to_rgba_zero_size_returns_empty_without_panicking() {
        let Some(ctx) = GpuContext::headless().ok() else { return };
        let mut r = Renderer::new(ctx.device, ctx.queue, wgpu::TextureFormat::Rgba8Unorm);
        let mut t = TextSystem::new();
        let l = DisplayList::new(8.0, 8.0, Color::WHITE);
        assert!(r.render_to_rgba(&l, &mut t, (0, 0), 1.0).is_empty());
        assert!(r.render_to_rgba(&l, &mut t, (0, 8), 1.0).is_empty());
        assert!(r.render_to_rgba(&l, &mut t, (8, 0), 1.0).is_empty());
        // A valid capture still works afterwards (the renderer stays usable).
        assert_eq!(r.render_to_rgba(&l, &mut t, (8, 8), 1.0).len(), 8 * 8 * 4);
    }

    // ---- Offscreen compositing layers --------------------------------------------------

    fn layer(rect: [f32; 4]) -> LayerInfo {
        LayerInfo { rect, radius: 0.0, opacity: 1.0, filter: Vec::new(), backdrop: Vec::new(), polygon: Vec::new() }
    }

    #[test]
    fn layer_composites_children_over_the_background() {
        let mut l = DisplayList::new(64.0, 64.0, Color::WHITE);
        l.push(rect(0.0, 0.0, 64.0, 64.0, Color::from_u8(0, 255, 0, 1.0)));
        l.push(Item::PushLayer(layer([16.0, 16.0, 32.0, 32.0])));
        l.push(rect(16.0, 16.0, 32.0, 32.0, Color::from_u8(255, 0, 0, 1.0)));
        l.push(Item::PopLayer);
        let Some(img) = render(&l, 64, 64) else { eprintln!("no GPU adapter, skipping"); return };
        assert_eq!(px(&img, 64, 32, 32), [255, 0, 0, 255], "layer child paints inside");
        assert_eq!(px(&img, 64, 4, 4), [0, 255, 0, 255], "background outside the layer stays");
        assert_eq!(px(&img, 64, 60, 60), [0, 255, 0, 255]);
    }

    #[test]
    fn layer_inside_a_transform_keeps_geometry() {
        let mut l = DisplayList::new(64.0, 64.0, Color::WHITE);
        l.push(Item::PushTransform([1.0, 0.0, 0.0, 1.0, 10.0, 0.0]));
        l.push(Item::PushLayer(layer([8.0, 8.0, 16.0, 16.0])));
        l.push(rect(8.0, 8.0, 16.0, 16.0, Color::BLACK));
        l.push(Item::PopLayer);
        l.push(Item::PopTransform);
        let Some(img) = render(&l, 64, 64) else { eprintln!("no GPU adapter, skipping"); return };
        assert_eq!(px(&img, 64, 22, 16), [0, 0, 0, 255], "layer child follows its ancestor transform");
        assert_eq!(px(&img, 64, 10, 16), [255, 255, 255, 255], "no paint left at the untransformed spot");
    }

    #[test]
    fn later_flat_run_paints_over_layer_in_display_order() {
        // [Flat A, Layer, Flat B]: B must land on top of the layer, and the layer's
        // backdrop must be snapshotted before B is painted. With deferred composites
        // submitted after all flat runs this painted B underneath the layer instead.
        let mut l = DisplayList::new(64.0, 64.0, Color::WHITE);
        l.push(rect(0.0, 0.0, 64.0, 64.0, Color::from_u8(0, 255, 0, 1.0)));
        l.push(Item::PushLayer(layer([16.0, 16.0, 32.0, 32.0])));
        l.push(rect(16.0, 16.0, 32.0, 32.0, Color::from_u8(0, 0, 255, 1.0)));
        l.push(Item::PopLayer);
        l.push(rect(24.0, 24.0, 16.0, 16.0, Color::from_u8(255, 0, 0, 1.0)));
        let Some(img) = render(&l, 64, 64) else { eprintln!("no GPU adapter, skipping"); return };
        assert_eq!(px(&img, 64, 32, 32), [255, 0, 0, 255], "later flat run is on top of the layer");
        assert_eq!(px(&img, 64, 20, 20), [0, 0, 255, 255], "layer child still shows outside the overlap");
        assert_eq!(px(&img, 64, 4, 4), [0, 255, 0, 255], "earlier flat run stays beneath");
    }

    #[test]
    fn backdrop_snapshot_excludes_later_flat_runs() {
        // A translucent layer child lets the backdrop through. The later green run is
        // opaque: if it leaked into the backdrop snapshot it would be mixed with the
        // translucent child instead of appearing at full strength on top.
        let mut l = DisplayList::new(64.0, 64.0, Color::WHITE);
        l.push(rect(0.0, 0.0, 32.0, 64.0, Color::BLACK));
        l.push(rect(32.0, 0.0, 32.0, 64.0, Color::WHITE));
        let mut info = layer([0.0, 0.0, 64.0, 64.0]);
        info.backdrop = vec![FilterOp::Blur(0.0)];
        l.push(Item::PushLayer(info));
        l.push(rect(0.0, 0.0, 64.0, 64.0, Color::from_u8(0, 0, 255, 0.5)));
        l.push(Item::PopLayer);
        l.push(rect(0.0, 0.0, 16.0, 64.0, Color::from_u8(0, 255, 0, 1.0)));
        let Some(img) = render(&l, 64, 64) else { eprintln!("no GPU adapter, skipping"); return };
        assert_eq!(px(&img, 64, 8, 32), [0, 255, 0, 255], "later run painted after the layer, undiluted");
        let right = px(&img, 64, 48, 32);
        assert!(right[2] > right[0] && right[2] > right[1], "backdrop is the prefix, not the later run: {right:?}");
    }

    #[test]
    fn pooled_layer_target_starts_transparent_each_frame() {
        let Some(ctx) = GpuContext::headless().ok() else { return };
        let mut r = Renderer::new(ctx.device, ctx.queue, wgpu::TextureFormat::Rgba8Unorm);
        let mut t = TextSystem::new();
        // Frame 1 fills the whole (padded) layer target with red.
        let mut a = DisplayList::new(64.0, 64.0, Color::WHITE);
        a.push(Item::PushLayer(layer([8.0, 8.0, 32.0, 32.0])));
        a.push(rect(8.0, 8.0, 32.0, 32.0, Color::from_u8(255, 0, 0, 1.0)));
        a.push(Item::PopLayer);
        let first = r.render_to_rgba(&a, &mut t, (64, 64), 1.0);
        assert_eq!(px(&first, 64, 12, 12), [255, 0, 0, 255]);
        // Frame 2 reuses the same target but only covers a small corner: everything
        // else must be transparent so the page background shows through.
        let mut b = DisplayList::new(64.0, 64.0, Color::WHITE);
        b.push(Item::PushLayer(layer([8.0, 8.0, 32.0, 32.0])));
        b.push(rect(32.0, 32.0, 8.0, 8.0, Color::from_u8(0, 0, 255, 1.0)));
        b.push(Item::PopLayer);
        let second = r.render_to_rgba(&b, &mut t, (64, 64), 1.0);
        assert_eq!(px(&second, 64, 12, 12), [255, 255, 255, 255], "no stale pixels in the reused layer target");
        assert_eq!(px(&second, 64, 36, 36), [0, 0, 255, 255], "new child still paints");
    }

    #[test]
    fn backdrop_filter_grayscale_desaturates_the_backdrop() {
        let mut l = DisplayList::new(64.0, 64.0, Color::WHITE);
        l.push(rect(0.0, 0.0, 64.0, 64.0, Color::from_u8(255, 0, 0, 1.0)));
        let mut info = layer([0.0, 0.0, 64.0, 64.0]);
        info.backdrop = vec![FilterOp::Grayscale(1.0)];
        l.push(Item::PushLayer(info));
        // Translucent content lets the filtered backdrop show through the layer.
        l.push(rect(0.0, 0.0, 64.0, 64.0, Color::from_u8(255, 255, 255, 0.5)));
        l.push(Item::PopLayer);
        let Some(img) = render(&l, 64, 64) else { eprintln!("no GPU adapter, skipping"); return };
        let p = px(&img, 64, 32, 32);
        assert!((p[0] as i32 - p[1] as i32).abs() <= 4 && (p[1] as i32 - p[2] as i32).abs() <= 4,
            "backdrop-filter:grayscale must make the backdrop neutral: {p:?}");
        assert!(p[0] < 210, "backdrop is grey, not red: {p:?}");
    }

    #[test]
    fn polygon_clip_cuts_per_pixel() {
        // A triangle covering the top-left half of the layer box.
        let mut info = layer([8.0, 8.0, 48.0, 48.0]);
        info.polygon = vec![[0.0, 0.0], [48.0, 0.0], [0.0, 48.0]];
        let mut l = DisplayList::new(64.0, 64.0, Color::WHITE);
        l.push(Item::PushLayer(info));
        l.push(rect(8.0, 8.0, 48.0, 48.0, Color::BLACK));
        l.push(Item::PopLayer);
        let Some(img) = render(&l, 64, 64) else { eprintln!("no GPU adapter, skipping"); return };
        assert_eq!(px(&img, 64, 18, 14), [0, 0, 0, 255], "inside the triangle");
        assert_eq!(px(&img, 64, 50, 50), [255, 255, 255, 255], "outside the triangle");
    }

    #[test]
    fn backdrop_filter_blurs_what_is_behind() {
        // Sharp black/white vertical stripes behind a translucent layer. Without a real
        // backdrop pass the stripes stay razor sharp; with blur the boundary is smoothed.
        let mut l = DisplayList::new(64.0, 64.0, Color::WHITE);
        for i in 0..8 {
            let c = if i % 2 == 0 { Color::BLACK } else { Color::WHITE };
            l.push(rect(i as f32 * 8.0, 0.0, 8.0, 64.0, c));
        }
        let mut info = layer([0.0, 0.0, 64.0, 64.0]);
        info.backdrop = vec![FilterOp::Blur(6.0)];
        l.push(Item::PushLayer(info));
        // Empty layer: nothing of its own, so only the filtered backdrop should show.
        l.push(Item::PopLayer);
        let Some(ctx) = GpuContext::headless().ok() else { return };
        let mut r = Renderer::new(ctx.device, ctx.queue, wgpu::TextureFormat::Rgba8Unorm);
        let mut t = TextSystem::new();
        let img = r.render_to_rgba(&l, &mut t, (64, 64), 1.0);
        assert_eq!(r.last_layers, 1, "one offscreen pass");
        // At x=8 (a stripe edge) the blurred result is grey, not pure black or white.
        let edge = px(&img, 64, 8, 32)[0];
        assert!(edge > 40 && edge < 215, "backdrop edge was blurred: {edge}");
        // Away from the edge the original colours still dominate.
        assert!(px(&img, 64, 2, 32)[0] < 90, "dark stripe stays dark: {:?}", px(&img, 64, 2, 32));
        assert!(px(&img, 64, 62, 32)[0] > 165, "light stripe stays light: {:?}", px(&img, 64, 62, 32));
    }

    #[test]
    fn translucent_backdrop_is_not_composited_twice() {
        // A translucent page background under a backdrop-filter layer used to be painted
        // twice: once inside `out = layer + backdrop*(1-out.a)` and again by the pipeline's
        // premultiplied blend over a framebuffer that already held the same backdrop.
        let mut l = DisplayList::new(64.0, 64.0, Color::from_u8(128, 0, 0, 0.5));
        let mut info = layer([0.0, 0.0, 64.0, 64.0]);
        info.backdrop = vec![FilterOp::Blur(0.0)]; // requests the backdrop pass, no blur
        l.push(Item::PushLayer(info));
        l.push(Item::PopLayer); // empty layer: the result is just the backdrop
        let Some(ctx) = GpuContext::headless().ok() else { return };
        let mut r = Renderer::new(ctx.device, ctx.queue, wgpu::TextureFormat::Rgba8Unorm);
        let mut t = TextSystem::new();
        let img = r.render_to_rgba(&l, &mut t, (64, 64), 1.0);
        // Exactly one copy of the translucent background: no brighter RGB (double blend)
        // and no higher alpha (0.5 + 0.5*0.5 = 0.75 under the old double-composite).
        for &(x, y) in &[(32, 32), (2, 2)] {
            let p = px(&img, 64, x, y);
            assert!((p[0] as i32 - 128).abs() <= 2 && p[1] == 0 && p[2] == 0 && (p[3] as i32 - 128).abs() <= 2,
                "backdrop applied once at ({x},{y}): {p:?}");
        }
    }

    #[test]
    fn multiple_backdrop_layers_sample_the_accumulated_prefix() {
        // The second layer's backdrop must include the first layer's result. The old code
        // re-rendered the whole prefix per layer; the new path snapshots the painted frame.
        let mut l = DisplayList::new(64.0, 64.0, Color::WHITE);
        l.push(rect(0.0, 0.0, 32.0, 64.0, Color::BLACK));
        // Layer 1: an opaque blue child, no backdrop filter.
        l.push(Item::PushLayer(layer([0.0, 0.0, 32.0, 64.0])));
        l.push(rect(0.0, 0.0, 32.0, 64.0, Color::from_u8(0, 0, 255, 1.0)));
        l.push(Item::PopLayer);
        // Layer 2: empty, but with a backdrop filter over the whole frame.
        let mut info = layer([0.0, 0.0, 64.0, 64.0]);
        info.backdrop = vec![FilterOp::Blur(0.0)];
        l.push(Item::PushLayer(info));
        l.push(Item::PopLayer);
        let Some(ctx) = GpuContext::headless().ok() else { return };
        let mut r = Renderer::new(ctx.device, ctx.queue, wgpu::TextureFormat::Rgba8Unorm);
        let mut t = TextSystem::new();
        let img = r.render_to_rgba(&l, &mut t, (64, 64), 1.0);
        assert_eq!(r.last_layers, 2, "two offscreen layers in one frame");
        assert_eq!(px(&img, 64, 16, 32), [0, 0, 255, 255], "layer 1's blue survives into layer 2's backdrop");
        assert_eq!(px(&img, 64, 48, 32), [255, 255, 255, 255], "unpainted half keeps the background");
    }

    #[test]
    fn target_pool_is_bounded_across_changing_layer_sizes() {
        let Some(ctx) = GpuContext::headless().ok() else { return };
        let mut r = Renderer::new(ctx.device, ctx.queue, wgpu::TextureFormat::Rgba8Unorm);
        let mut t = TextSystem::new();
        for i in 0..(MAX_TARGETS * 2) as u32 {
            let w = 8.0 + (i % 17) as f32 * 3.0;
            let mut l = DisplayList::new(64.0, 64.0, Color::WHITE);
            l.push(Item::PushLayer(layer([0.0, 0.0, w, 16.0])));
            l.push(rect(0.0, 0.0, w, 16.0, Color::from_u8(255, 0, 0, 1.0)));
            l.push(Item::PopLayer);
            let _ = r.render_to_rgba(&l, &mut t, (64, 64), 1.0);
            assert!(r.target_pool_len() <= MAX_TARGETS, "idle target pool exceeded cap on frame {i}: {}", r.target_pool_len());
        }
    }

    #[test]
    fn lru_order_is_newest_first() {
        use cosmic_text::{CacheKey, CacheKeyFlags};
        let key = |glyph: u16| CacheKey::new(cosmic_text::fontdb::ID::dummy(), glyph, 16.0, (0.0, 0.0), CacheKeyFlags::empty()).0;
        let mut recency = HashMap::new();
        recency.insert(key(1), 10u64);
        recency.insert(key(2), 30u64);
        recency.insert(key(3), 20u64);
        let order = lru_order(&recency);
        assert_eq!(order.iter().map(|(k, _)| k.glyph_id).collect::<Vec<_>>(), vec![2, 3, 1]);
        assert!(order.windows(2).all(|w| w[0].1 >= w[1].1), "monotonic recency");
        assert!(lru_order(&HashMap::new()).is_empty());
    }

    #[test]
    fn text_culling_keeps_anything_possibly_visible() {
        let clip = [0.0, 0.0, 100.0, 50.0];
        assert!(text_maybe_visible(10.0, 10.0, 20.0, clip));
        // Edge-touching counts as visible (conservative).
        assert!(text_maybe_visible(-20.0, 10.0, 20.0, clip));
        assert!(text_maybe_visible(100.0, 10.0, 20.0, clip));
        // Fully outside left / right / below is skipped.
        assert!(!text_maybe_visible(-30.0, 10.0, 20.0, clip));
        assert!(!text_maybe_visible(110.0, 10.0, 20.0, clip));
        assert!(!text_maybe_visible(10.0, 60.0, 20.0, clip));
        // Above is never culled: the height is unknown pre-shape.
        assert!(text_maybe_visible(10.0, -100.0, 20.0, clip));
    }
}
