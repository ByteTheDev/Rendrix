//! Rendrix graphics: the display list, text shaping, the wgpu renderer, and Canvas 2D raster state.

pub mod canvas;
pub mod context;
pub mod display;
pub mod renderer;
pub mod scene3d;
pub mod svg;
pub mod text;

pub use context::GpuContext;
pub use display::*;
pub use renderer::Renderer;
pub use scene3d::{remove_scene, scene_ids, upsert_scene};
pub use text::TextSystem;
