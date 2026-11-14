//! The display list: what to draw, in paint order, independent of how the GPU draws it.

use rendrix_css::animation::Matrix;
use rendrix_css::Color;
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

#[derive(Debug, Clone, PartialEq)]
pub struct RectItem {
    /// x, y, width, height in logical pixels.
    pub rect: [f32; 4],
    /// Corner radii: top-left, top-right, bottom-right, bottom-left.
    pub radii: [f32; 4],
    pub fill: Fill,
    /// Border widths [top, right, bottom, left] and color.
    pub border: Option<([f32; 4], Color)>,
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

#[derive(Debug, Clone)]
pub enum Item {
    Rect(RectItem),
    Shadow(ShadowItem),
    Text(TextItem),
    Image(ImageItem),
    /// Clip subsequent items to a rounded rectangle until the matching `PopClip`.
    PushClip { rect: [f32; 4], radius: f32 },
    PopClip,
    /// Multiply the current transform (logical pixels) until `PopTransform`.
    PushTransform(Matrix),
    PopTransform,
    /// Multiply alpha of everything drawn until `PopOpacity` (per-item, not an offscreen group).
    PushOpacity(f32),
    PopOpacity,
}

#[derive(Debug, Clone)]
pub struct DisplayList {
    /// Logical viewport size.
    pub width: f32,
    pub height: f32,
    pub background: Color,
    pub items: Vec<Item>,
}

impl DisplayList {
    pub fn new(width: f32, height: f32, background: Color) -> Self {
        DisplayList { width, height, background, items: Vec::new() }
    }
    pub fn push(&mut self, item: Item) {
        self.items.push(item);
    }
}
