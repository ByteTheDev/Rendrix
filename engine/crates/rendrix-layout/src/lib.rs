//! Rendrix layout: a layout-engine-neutral box tree ([`LayoutNode`]) and a [`LayoutEngine`] trait.
//!
//! The only implementation today is [`TaffyEngine`]. It keeps its Taffy tree between frames and only
//! touches nodes whose style, children or text content changed, so unchanged subtrees reuse Taffy's
//! layout cache (incremental recalculation).
//!
//! Text is measured through [`TextMeasurer`]; this crate never shapes text itself.

mod taffy_engine;

pub use taffy_engine::TaffyEngine;

use rendrix_css::ComputedStyle;
use rendrix_css::LineHeight;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::sync::Arc;

/// One styled span of text inside an inline formatting context.
#[derive(Debug, Clone)]
pub struct InlineRun {
    pub text: String,
    pub style: Arc<ComputedStyle>,
    /// DOM node that produced the run (for hit testing / selection); 0 if synthetic.
    pub node: u64,
    /// A forced line break (`<br>`).
    pub is_break: bool,
}

/// The inline content of a block: runs flowed and wrapped as one paragraph.
#[derive(Debug, Clone)]
pub struct InlineContent {
    pub runs: Vec<InlineRun>,
    /// The block's own style: text-align, white-space, line-height strut.
    pub container: Arc<ComputedStyle>,
    /// Memoized [`InlineContent::key`]. Content is built once per change and never
    /// mutated in place (edits construct a fresh `InlineContent`), so hashing once
    /// per object caps full-content hashing at one per text item per frame max:
    /// layout sync, Taffy measures and paint all share the memoized value.
    key_cache: std::sync::OnceLock<u64>,
}

fn hash_line_height(h: &LineHeight, state: &mut impl Hasher) {
    // Hash the enum structurally instead of `format!("{:?}")`: no allocation.
    match h {
        LineHeight::Normal => 0u8.hash(state),
        LineHeight::Number(n) => {
            1u8.hash(state);
            n.to_bits().hash(state);
        }
        LineHeight::Px(p) => {
            2u8.hash(state);
            p.to_bits().hash(state);
        }
    }
}

impl InlineContent {
    /// Build inline content with an empty key cache.
    pub fn new(runs: Vec<InlineRun>, container: Arc<ComputedStyle>) -> Self {
        InlineContent { runs, container, key_cache: std::sync::OnceLock::new() }
    }

    /// A hash over everything that affects measurement. Equal keys ⇒ equal measurements.
    ///
    /// The full content is hashed at most once per object; the result is memoized
    /// (see `key_cache`), so repeated layout/paint queries are `O(1)`.
    pub fn key(&self) -> u64 {
        *self.key_cache.get_or_init(|| self.compute_key())
    }

    fn compute_key(&self) -> u64 {
        let mut h = std::collections::hash_map::DefaultHasher::new();
        for r in &self.runs {
            r.text.hash(&mut h);
            r.is_break.hash(&mut h);
            let s = &r.style;
            s.font_family.hash(&mut h);
            s.font_size.to_bits().hash(&mut h);
            s.font_weight.hash(&mut h);
            (s.font_style as u8).hash(&mut h);
            s.letter_spacing.to_bits().hash(&mut h);
            s.word_spacing.to_bits().hash(&mut h);
            (s.text_transform as u8).hash(&mut h);
            hash_line_height(&s.line_height, &mut h);
        }
        let c = &self.container;
        (c.white_space as u8).hash(&mut h);
        (c.text_align as u8).hash(&mut h);
        c.text_overflow_ellipsis.hash(&mut h);
        hash_line_height(&c.line_height, &mut h);
        c.font_size.to_bits().hash(&mut h);
        h.finish()
    }
}

/// Measures inline content. Implemented by the text system.
pub trait TextMeasurer {
    /// Size of `content` wrapped at `max_width` (`None` = no wrapping constraint / max-content).
    fn measure(&mut self, content: &InlineContent, max_width: Option<f32>) -> (f32, f32);
}

#[derive(Debug, Clone)]
pub enum LayoutKind {
    Box,
    Text(Arc<InlineContent>),
    /// An image-like box with an intrinsic size.
    Replaced { width: f32, height: f32 },
}

#[derive(Debug, Clone)]
pub struct LayoutNode {
    /// Stable identity across frames (DOM node id for element boxes).
    pub id: u64,
    pub style: Arc<ComputedStyle>,
    pub kind: LayoutKind,
    pub children: Vec<LayoutNode>,
}

/// Resolved geometry of one box. `x`/`y` are relative to the parent's border-box origin.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct BoxLayout {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
    /// Scrollable content extent (≥ box size minus borders when content overflows).
    pub content_width: f32,
    pub content_height: f32,
    /// Resolved border widths [top, right, bottom, left].
    pub border: [f32; 4],
    /// Resolved padding [top, right, bottom, left].
    pub padding: [f32; 4],
}

#[derive(Debug, Default)]
pub struct LayoutResult {
    pub boxes: HashMap<u64, BoxLayout>,
}

pub trait LayoutEngine {
    fn layout(&mut self, root: &LayoutNode, viewport: (f32, f32), measurer: &mut dyn TextMeasurer) -> LayoutResult;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn styled(text: &str) -> InlineContent {
        let s = Arc::new(ComputedStyle::default());
        InlineContent::new(vec![InlineRun { text: text.into(), style: s.clone(), node: 1, is_break: false }], s)
    }

    #[test]
    fn key_is_stable_and_memoized() {
        let c = styled("hello");
        let k1 = c.key();
        let k2 = c.key();
        assert_eq!(k1, k2);
        // Clone carries the memoized value: equal content, equal key, no rehash.
        let d = c.clone();
        assert_eq!(d.key(), k1);
        // Distinct text ⇒ distinct key.
        assert_ne!(styled("world").key(), k1);
    }

    #[test]
    fn key_covers_style_and_breaks() {
        let base = styled("hi");
        // Fresh object (never keyed) with a different size hashes differently.
        let s2 = {
            let mut s = ComputedStyle::default();
            s.font_size = 99.0;
            Arc::new(s)
        };
        let other = InlineContent::new(
            vec![InlineRun { text: "hi".into(), style: s2.clone(), node: 1, is_break: false }],
            s2,
        );
        assert_ne!(other.key(), base.key());
        // Line-height variants hash without Debug formatting.
        let mut lh = ComputedStyle::default();
        lh.line_height = LineHeight::Number(2.0);
        let lh = Arc::new(lh);
        let lh_content = InlineContent::new(vec![InlineRun { text: "hi".into(), style: lh.clone(), node: 1, is_break: false }], lh);
        assert_ne!(lh_content.key(), base.key());
    }
}
