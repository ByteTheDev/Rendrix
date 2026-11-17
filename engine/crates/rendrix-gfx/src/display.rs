//! The display list: what to draw, in paint order, independent of how the GPU draws it.

use rendrix_css::animation::Matrix;
use rendrix_css::{Color, FilterOp};
use rendrix_layout::InlineContent;
use std::sync::Arc;

pub type ImageId = u32;

/// Canvas compositing: a `<canvas>` is one more textured quad — it reuses `Item::Image` with
/// this id, so the existing clip/transform/opacity flattening applies unchanged. Each visible
/// canvas binds its own texture, which may add **one batch per visible canvas** to
/// `Renderer::last_batches` (documented cost of the A3 subset; DOM content still batches as
/// before when no canvas is present).
pub fn canvas_image_id(node: u32) -> ImageId {
    0x8000_0000 | (node & 0x7FFF_FFFF)
}

#[derive(Debug, Clone, PartialEq)]
pub enum Fill {
    None,
    Solid(Color),
    /// Angle in radians (CSS convention: 0 = to top). Up to four stops are rendered.
    Linear { angle: f32, stops: Vec<(Color, f32)> },
    Radial { stops: Vec<(Color, f32)> },
}

/// A rectangle's border, with independent width, colour and style per side.
/// Side order is [top, right, bottom, left]. Style codes match the shader:
/// 0 = solid, 1 = dashed, 2 = dotted, 3 = double (double falls back to solid).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Border {
    pub widths: [f32; 4],
    pub colors: [Color; 4],
    pub styles: [u8; 4],
}

impl Border {
    pub fn solid(widths: [f32; 4], color: Color) -> Border {
        Border { widths, colors: [color; 4], styles: [0; 4] }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct RectItem {
    /// x, y, width, height in logical pixels.
    pub rect: [f32; 4],
    /// Corner radii: top-left, top-right, bottom-right, bottom-left.
    pub radii: [f32; 4],
    pub fill: Fill,
    pub border: Option<Border>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ShadowItem {
    /// The shadow shape (already offset and spread-expanded for outer shadows; the padding box for inset).
    pub rect: [f32; 4],
    pub radii: [f32; 4],
    pub blur: f32,
    pub color: Color,
    pub inset: bool,
    /// For inset shadows: the "hole" the shadow is cast around (offset, spread-shrunk).
    pub hole: [f32; 4],
}

#[derive(Debug, Clone)]
pub struct TextItem {
    pub content: Arc<InlineContent>,
    /// Top-left of the text box in logical pixels.
    pub x: f32,
    pub y: f32,
    /// Width the paragraph is laid out at.
    pub width: f32,
}

#[derive(Debug, Clone)]
pub struct ImageItem {
    pub image: ImageId,
    pub rect: [f32; 4],
    pub radii: [f32; 4],
    /// Sub-rectangle of the image to show, in normalised UV (x, y, w, h).
    pub uv: [f32; 4],
}

/// A 3D scene viewport: the GPU scene registered under `scene_id` (the hosting
/// element's DOM id) composites into `rect`. Missing scenes paint nothing, so a
/// stale item is transparent, never wrong.
#[derive(Debug, Clone, PartialEq)]
pub struct Scene3DItem {
    pub scene_id: u64,
    /// Border-box rect in logical pixels.
    pub rect: [f32; 4],
}

/// How an isolated compositing layer is combined with its backdrop.
///
/// A layer is rendered into its own offscreen texture and composited as one quad. This is
/// the foundation for effects that need to read pixels (rather than fold into per-instance
/// factors): `backdrop-filter`, true `filter: blur()`, per-pixel `clip-path: polygon()` and
/// group opacity. Layers are opt-in: frames without one keep the single-draw-call fast path.
#[derive(Debug, Clone)]
pub struct LayerInfo {
    /// Border box in logical pixels. The offscreen target is sized from its device-space AABB.
    pub rect: [f32; 4],
    /// Border radius in logical pixels (applied when compositing the layer).
    pub radius: f32,
    /// Group opacity applied once to the whole composited layer.
    pub opacity: f32,
    /// Filters applied to the layer's own content (before it is composited).
    pub filter: Vec<FilterOp>,
    /// Filters applied to the backdrop *behind* the layer (`backdrop-filter`).
    pub backdrop: Vec<FilterOp>,
    /// Per-pixel clip polygon, points in logical pixels relative to `rect.xy`.
    /// Empty means no polygon clip.
    pub polygon: Vec<[f32; 2]>,
}

impl LayerInfo {
    /// True when this layer actually needs an offscreen pass (no redundant layers).
    pub fn needs_pass(&self) -> bool {
        // Group opacity alone is enough: children blend with each other first and the
        // isolated result composites once at `opacity`. Blur/backdrop/polygon read pixels
        // and likewise require an offscreen target.
        self.opacity < 1.0
            || !self.backdrop.is_empty()
            || self.filter.iter().any(|op| matches!(op, FilterOp::Blur(r) if *r > 0.0))
            || !self.polygon.is_empty()
    }
}

#[derive(Debug, Clone)]
pub enum Item {
    /// Render everything up to the matching `PopLayer` into an offscreen texture and
    /// composite it with `LayerInfo`'s effects. Nested layers are supported.
    PushLayer(LayerInfo),
    PopLayer,
    Rect(RectItem),
    Shadow(ShadowItem),
    Text(TextItem),
    Image(ImageItem),
    Scene3D(Scene3DItem),
    /// Clip subsequent items to a rounded rectangle until the matching `PopClip`.
    PushClip { rect: [f32; 4], radius: f32 },
    /// Clip subsequent items to the ellipse inscribed in `rect` until `PopClip`.
    /// (`clip-path: circle()` / `ellipse()`; paint resolves centres/radii to a box.)
    PushEllipseClip { rect: [f32; 4] },
    PopClip,
    /// Multiply the current transform (logical pixels) until `PopTransform`.
    PushTransform(Matrix),
    PopTransform,
    /// Multiply alpha of everything drawn until `PopOpacity` (per-item, not an offscreen group).
    PushOpacity(f32),
    PopOpacity,
    /// Apply `filter` functions to subsequent items until `PopFilter`. Brightness and
    /// grayscale are exact pointwise factors. CSS `filter: blur()` goes through the
    /// offscreen `PushLayer` path (a true Gaussian in the composite shader); the flat-path
    /// offset-copy expansion here is only a fallback for blur that arrives without a layer.
    PushFilter(Vec<FilterOp>),
    PopFilter,
    /// `backdrop-filter` marker until `PopBackdrop`. CSS `backdrop-filter` is rendered by a
    /// real offscreen backdrop-filter pass via `PushLayer`; this marker only maintains the
    /// flat-path filter stack for items emitted outside a layer.
    PushBackdrop(Vec<FilterOp>),
    PopBackdrop,
}

#[derive(Debug, Clone)]
pub struct DisplayList {
    /// Logical viewport size.
    pub width: f32,
    pub height: f32,
    pub background: Color,
    pub items: Vec<Item>,
    /// Set when any [`Item::Scene3D`] is present; lets the renderer skip all 3D
    /// work (no store lock, no depth texture) on pure-2D frames.
    pub has_3d: bool,
    /// Set when any [`Item::PushLayer`] is present; lets the renderer skip all
    /// offscreen-target allocation on ordinary frames.
    pub has_layers: bool,
}

impl DisplayList {
    pub fn new(width: f32, height: f32, background: Color) -> Self {
        DisplayList { width, height, background, items: Vec::new(), has_3d: false, has_layers: false }
    }
    pub fn push(&mut self, item: Item) {
        if matches!(item, Item::Scene3D(_)) {
            self.has_3d = true;
        }
        if matches!(item, Item::PushLayer(_)) {
            self.has_layers = true;
        }
        self.items.push(item);
    }
}
