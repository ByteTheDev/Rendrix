//! Text shaping and measurement on top of cosmic-text, with a paragraph cache.
//!
//! A [`Paragraph`] is the shaped, wrapped result for one `(content, width)` pair. Measurement during
//! layout and drawing during paint hit the same cache, so text is shaped once per change.

use cosmic_text::{Align, Attrs, Buffer, CacheKey, Family, FontSystem, Metrics, Shaping, Style, SwashCache, SwashContent, Weight, Wrap};
use rendrix_css::{FontStyle, TextAlign, TextTransform, WhiteSpace};
use rendrix_layout::{InlineContent, TextMeasurer};
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

pub struct GlyphInfo {
    pub glyph: cosmic_text::LayoutGlyph,
    /// Baseline of the line, relative to the paragraph's top.
    pub line_y: f32,
    pub run: usize,
}

/// A horizontal span of one run on one line (used for decorations and selection geometry).
#[derive(Debug, Clone, Copy)]
pub struct Segment {
    pub run: usize,
    pub x: f32,
    pub w: f32,
    pub line_y: f32,
    pub line_top: f32,
    pub line_height: f32,
}

pub struct Paragraph {
    pub width: f32,
    pub height: f32,
    pub glyphs: Vec<GlyphInfo>,
    pub segments: Vec<Segment>,
    pub text: String,
    buffer: Buffer,
    /// Byte offset in `text` where each cosmic line (split on '\n') starts.
    line_starts: Vec<usize>,
    /// Width constraint the buffer was shaped with (`None` = unconstrained).
    /// Paint/layout reuse a cached paragraph only for requested widths this
    /// fits, so coarse cache keys can never produce an overflowing paragraph.
    pub wrap: Option<f32>,
}

pub struct Raster {
    pub width: u32,
    pub height: u32,
    pub left: i32,
    pub top: i32,
    pub is_color: bool,
    /// RGBA8, premultiplied alpha.
    pub rgba: Vec<u8>,
}

#[derive(Hash, PartialEq, Eq, Clone, Copy)]
struct ParaKey {
    content: u64,
    width_bits: u32,
}

/// Paragraph cache capacity. When full the least-recently-used entry is evicted
/// (never a full `clear()`, so a shaping storm cannot drop the whole hot set).
pub const PARA_CACHE_CAP: usize = 2048;
/// Width quantisation step for the paragraph cache: widths falling in the same
/// step share one shaping. Shaping always uses the step's floor, so a cached
/// paragraph was wrapped at a width ≤ the requested one and can never overflow
/// it (at most it keeps one extra break near a bucket boundary).
pub const PARA_CACHE_QUANT: f32 = 0.5;

/// Quantise a layout width down to the cache grid. Non-finite/negative widths
/// (should not happen) map to zero instead of producing garbage keys.
fn quantize_width(w: f32) -> f32 {
    if !w.is_finite() {
        return 0.0;
    }
    ((w / PARA_CACHE_QUANT).floor() * PARA_CACHE_QUANT).max(0.0)
}

fn width_bits(q: f32) -> u32 {
    // `q` is already a multiple of PARA_CACHE_QUANT, so this is exact for any
    // realistic viewport width.
    ((q / PARA_CACHE_QUANT).round() as u32).wrapping_add(1)
}

struct CacheEntry {
    para: Rc<Paragraph>,
    last_used: u64,
}

pub struct TextSystem {
    pub fonts: FontSystem,
    swash: SwashCache,
    cache: HashMap<ParaKey, CacheEntry>,
    clock: u64,
    installed: HashSet<String>,
    /// Stats for tests/profiling.
    pub shape_count: usize,
    /// Paragraph-cache lookups served without shaping.
    pub cache_hits: usize,
}

impl Default for TextSystem {
    fn default() -> Self {
        TextSystem::new()
    }
}

fn transform_text(s: &str, t: TextTransform) -> String {
    match t {
        TextTransform::None => s.to_string(),
        TextTransform::Uppercase => s.to_uppercase(),
        TextTransform::Lowercase => s.to_lowercase(),
        TextTransform::Capitalize => {
            let mut out = String::with_capacity(s.len());
            let mut start = true;
            for c in s.chars() {
                if start && c.is_alphabetic() {
                    out.extend(c.to_uppercase());
                    start = false;
                } else {
                    out.push(c);
                    start = c.is_whitespace();
                }
            }
            out
        }
    }
}

impl TextSystem {
    pub fn new() -> Self {
        let fonts = FontSystem::new();
        let installed = fonts
            .db()
            .faces()
            .flat_map(|f| f.families.iter().map(|(n, _)| n.to_lowercase()).collect::<Vec<_>>())
            .collect();
        TextSystem { fonts, swash: SwashCache::new(), cache: HashMap::new(), clock: 0, installed, shape_count: 0, cache_hits: 0 }
    }

    /// Number of paragraphs currently cached.
    pub fn cache_len(&self) -> usize {
        self.cache.len()
    }

    /// Hit rate over `paragraph()` lookups (`hits / (hits + shapes)`).
    pub fn hit_rate(&self) -> f64 {
        let total = self.cache_hits + self.shape_count;
        if total == 0 {
            1.0
        } else {
            self.cache_hits as f64 / total as f64
        }
    }

    /// Register an application font (TTF/OTF bytes).
    pub fn load_font(&mut self, data: Vec<u8>) {
        self.fonts.db_mut().load_font_data(data);
        self.installed = self
            .fonts
            .db()
            .faces()
            .flat_map(|f| f.families.iter().map(|(n, _)| n.to_lowercase()).collect::<Vec<_>>())
            .collect();
        self.cache.clear();
    }

    fn resolve_family<'a>(&self, list: &'a [String]) -> Family<'a> {
        for name in list {
            match name.to_ascii_lowercase().as_str() {
                "sans-serif" | "system-ui" | "ui-sans-serif" | "-apple-system" | "blinkmacsystemfont" => return Family::SansSerif,
                "serif" | "ui-serif" => return Family::Serif,
                "monospace" | "ui-monospace" => return Family::Monospace,
                "cursive" => return Family::Cursive,
                "fantasy" => return Family::Fantasy,
                other => {
                    if self.installed.contains(other) {
                        return Family::Name(name.as_str());
                    }
                }
            }
        }
        Family::SansSerif
    }

    fn build(&mut self, content: &InlineContent, width: Option<f32>) -> Paragraph {
        self.shape_count += 1;
        let c = &content.container;
        let metrics = Metrics::new(c.font_size.max(1.0), c.line_height.resolve(c.font_size).max(1.0));
        let mut buffer = Buffer::new(&mut self.fonts, metrics);
        let nowrap = matches!(c.white_space, WhiteSpace::NoWrap | WhiteSpace::Pre);
        buffer.set_wrap(&mut self.fonts, if nowrap { Wrap::None } else { Wrap::WordOrGlyph });
        buffer.set_size(&mut self.fonts, if nowrap { None } else { width }, None);

        // Keep owned strings alive for the span borrow.
        let texts: Vec<String> = content
            .runs
            .iter()
            .map(|r| if r.is_break { "\n".to_string() } else { transform_text(&r.text, r.style.text_transform) })
            .collect();
        let families: Vec<Family> = content.runs.iter().map(|r| self.resolve_family(&r.style.font_family)).collect();
        let spans: Vec<(&str, Attrs)> = content
            .runs
            .iter()
            .enumerate()
            .map(|(i, r)| {
                let s = &r.style;
                let mut a = Attrs::new()
                    .family(families[i])
                    .weight(Weight(s.font_weight))
                    .style(match s.font_style {
                        FontStyle::Normal => Style::Normal,
                        FontStyle::Italic => Style::Italic,
                        FontStyle::Oblique => Style::Oblique,
                    })
                    .metadata(i)
                    .metrics(Metrics::new(s.font_size.max(1.0), s.line_height.resolve(s.font_size).max(1.0)));
                if s.letter_spacing != 0.0 {
                    a = a.letter_spacing(s.letter_spacing / s.font_size.max(1.0));
                }
                (texts[i].as_str(), a)
            })
            .collect();
        let align = match c.text_align {
            TextAlign::Left | TextAlign::Start => Align::Left,
            TextAlign::Right | TextAlign::End => Align::Right,
            TextAlign::Center => Align::Center,
            TextAlign::Justify => Align::Justified,
        };
        let default_attrs = Attrs::new();
        buffer.set_rich_text(&mut self.fonts, spans, &default_attrs, Shaping::Advanced, Some(align));
        buffer.shape_until_scroll(&mut self.fonts, false);

        let text: String = texts.concat();
        let mut line_starts = vec![0usize];
        for (i, b) in text.bytes().enumerate() {
            if b == b'\n' {
                line_starts.push(i + 1);
            }
        }
        let mut glyphs = Vec::new();
        let mut segments: Vec<Segment> = Vec::new();
        let mut max_w = 0.0f32;
        let mut height = 0.0f32;
        for run in buffer.layout_runs() {
            max_w = max_w.max(run.line_w);
            height = height.max(run.line_top + run.line_height);
            let mut cur: Option<Segment> = None;
            for g in run.glyphs {
                let seg_run = g.metadata;
                match &mut cur {
                    Some(s) if s.run == seg_run => {
                        s.w = (g.x + g.w) - s.x;
                    }
                    _ => {
                        if let Some(s) = cur.take() {
                            segments.push(s);
                        }
                        cur = Some(Segment { run: seg_run, x: g.x, w: g.w, line_y: run.line_y, line_top: run.line_top, line_height: run.line_height });
                    }
                }
                glyphs.push(GlyphInfo { glyph: g.clone(), line_y: run.line_y, run: seg_run });
            }
            if let Some(s) = cur.take() {
                segments.push(s);
            }
        }
        // Empty content has no height; an explicit trailing newline still occupies a line.
        Paragraph { width: max_w, height, glyphs, segments, text, buffer, line_starts, wrap: width }
    }

    pub fn paragraph(&mut self, content: &InlineContent, width: Option<f32>) -> Rc<Paragraph> {
        // When the text fits on one line and is left-aligned, the unconstrained layout is identical at
        // any wider width: share it instead of shaping again.
        if let Some(w) = width {
            if matches!(content.container.text_align, TextAlign::Left | TextAlign::Start) {
                let natural = self.paragraph(content, None);
                if natural.width <= w && natural.glyphs.iter().map(|g| g.line_y.to_bits()).collect::<std::collections::HashSet<_>>().len() <= 1 {
                    return natural;
                }
            }
        }
        // Coarse width key: resize/scroll drags sweep many nearby widths, and shaping
        // at the bucket floor keeps every width in the bucket sharing one paragraph
        // that provably fits (constraint ≤ requested width).
        let qw = width.map(quantize_width);
        let key = ParaKey { content: content.key(), width_bits: qw.map(width_bits).unwrap_or(0) };
        if let Some(e) = self.cache.get_mut(&key) {
            self.clock = self.clock.wrapping_add(1);
            e.last_used = self.clock;
            self.cache_hits += 1;
            return e.para.clone();
        }
        if self.cache.len() >= PARA_CACHE_CAP {
            // Least-recently-used eviction of a single entry: the hot set survives a
            // shaping storm instead of being dropped by a full clear.
            if let Some(victim) = self.cache.iter().min_by_key(|(_, e)| e.last_used).map(|(k, _)| *k) {
                self.cache.remove(&victim);
            }
        }
        let p = Rc::new(self.build(content, qw));
        self.clock = self.clock.wrapping_add(1);
        self.cache.insert(key, CacheEntry { para: p.clone(), last_used: self.clock });
        p
    }

    /// Caret position (x, top y, height) for a byte offset into the concatenated text.
    pub fn caret(&mut self, content: &InlineContent, width: Option<f32>, byte: usize) -> (f32, f32, f32) {
        let p = self.paragraph(content, width);
        let fallback_h = content.container.line_height.resolve(content.container.font_size);
        self.caret_in(&p, byte, fallback_h)
    }

    /// [`caret`](Self::caret) against an already-resolved paragraph, so paint can
    /// reuse one `paragraph()` lookup for caret + selection + hit segments instead
    /// of re-hashing the full text per query.
    pub fn caret_in(&self, p: &Paragraph, byte: usize, fallback_h: f32) -> (f32, f32, f32) {
        let byte = byte.min(p.text.len());
        let line = p.line_starts.iter().rposition(|s| *s <= byte).unwrap_or(0);
        let idx = byte - p.line_starts[line];
        let mut best: Option<(f32, f32, f32)> = None;
        for run in p.buffer.layout_runs() {
            if run.line_i != line {
                continue;
            }
            let mut x = None;
            for g in run.glyphs {
                if g.start <= idx && idx < g.end {
                    x = Some(g.x);
                    break;
                }
            }
            let x = x.unwrap_or_else(|| run.glyphs.last().map(|g| g.x + g.w).unwrap_or(0.0));
            let at_end = run.glyphs.last().is_none_or(|g| idx >= g.end);
            best = Some((x, run.line_top, run.line_height));
            if !at_end {
                break;
            }
        }
        best.unwrap_or((0.0, 0.0, fallback_h))
    }

    /// Byte offset nearest to a point inside the paragraph.
    pub fn hit(&mut self, content: &InlineContent, width: Option<f32>, x: f32, y: f32) -> usize {
        let p = self.paragraph(content, width);
        self.hit_in(&p, x, y)
    }

    /// [`hit`](Self::hit) against an already-resolved paragraph.
    pub fn hit_in(&self, p: &Paragraph, x: f32, y: f32) -> usize {
        match p.buffer.hit(x, y) {
            Some(c) => p.line_starts.get(c.line).copied().unwrap_or(0) + c.index,
            None => p.text.len(),
        }
    }

    /// Selection rectangles (x, y, w, h) for the byte range, in paragraph coordinates.
    pub fn selection_rects(&mut self, content: &InlineContent, width: Option<f32>, start: usize, end: usize) -> Vec<[f32; 4]> {
        let p = self.paragraph(content, width);
        self.selection_rects_in(&p, start, end)
    }

    /// [`selection_rects`](Self::selection_rects) against an already-resolved paragraph.
    pub fn selection_rects_in(&self, p: &Paragraph, start: usize, end: usize) -> Vec<[f32; 4]> {
        let (start, end) = (start.min(end), start.max(end));
        let mut out = Vec::new();
        for run in p.buffer.layout_runs() {
            let base = p.line_starts.get(run.line_i).copied().unwrap_or(0);
            let (mut x0, mut x1): (Option<f32>, f32) = (None, 0.0);
            for g in run.glyphs {
                let (gs, ge) = (base + g.start, base + g.end);
                if ge > start && gs < end {
                    x0.get_or_insert(g.x);
                    x1 = g.x + g.w;
                }
            }
            if let Some(x0) = x0 {
                out.push([x0, run.line_top, x1 - x0, run.line_height]);
            }
        }
        out
    }

    /// Rasterize a glyph at its cache key's size. `None` for blank glyphs (spaces).
    pub fn raster(&mut self, key: CacheKey) -> Option<Raster> {
        let img = self.swash.get_image(&mut self.fonts, key).as_ref()?;
        let (w, h) = (img.placement.width, img.placement.height);
        if w == 0 || h == 0 {
            return None;
        }
        let mut rgba = Vec::with_capacity((w * h * 4) as usize);
        let is_color = matches!(img.content, SwashContent::Color);
        match img.content {
            SwashContent::Mask => {
                for a in &img.data {
                    // Premultiplied white; tint is applied in the shader.
                    rgba.extend_from_slice(&[*a, *a, *a, *a]);
                }
            }
            SwashContent::SubpixelMask => {
                for px in img.data.chunks_exact(3) {
                    let a = ((px[0] as u32 + px[1] as u32 + px[2] as u32) / 3) as u8;
                    rgba.extend_from_slice(&[a, a, a, a]);
                }
            }
            SwashContent::Color => {
                for px in img.data.chunks_exact(4) {
                    let a = px[3] as u32;
                    rgba.extend_from_slice(&[(px[0] as u32 * a / 255) as u8, (px[1] as u32 * a / 255) as u8, (px[2] as u32 * a / 255) as u8, px[3]]);
                }
            }
        }
        Some(Raster { width: w, height: h, left: img.placement.left, top: img.placement.top, is_color, rgba })
    }
}

impl TextMeasurer for TextSystem {
    fn measure(&mut self, content: &InlineContent, max_width: Option<f32>) -> (f32, f32) {
        if content.runs.iter().all(|r| r.text.is_empty() && !r.is_break) {
            return (0.0, 0.0);
        }
        let p = self.paragraph(content, max_width);
        // A wrapped paragraph is as wide as its widest line; ceil avoids sub-pixel re-wrapping at the final size.
        (p.width.ceil(), p.height.ceil())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rendrix_css::ComputedStyle;
    use rendrix_layout::InlineRun;
    use std::sync::Arc;

    fn content(text: &str, size: f32) -> InlineContent {
        let mut s = ComputedStyle::default();
        s.font_size = size;
        let s = Arc::new(s);
        InlineContent { runs: vec![InlineRun { text: text.into(), style: s.clone(), node: 1, is_break: false }], container: s }
    }

    #[test]
    fn measures_and_wraps() {
        let mut t = TextSystem::new();
        let c = content("hello world this wraps over several lines", 16.0);
        let (w_full, h_one) = t.measure(&c, None);
        assert!(w_full > 100.0 && h_one > 10.0 && h_one < 40.0, "single line: {w_full}x{h_one}");
        let (w, h) = t.measure(&c, Some(w_full / 3.0));
        assert!(w <= w_full / 3.0 + 1.0, "wrapped width {w} within limit");
        assert!(h >= h_one * 2.5, "wrapped to several lines: {h} vs {h_one}");
        let (_, h_big) = t.measure(&content("hello world this wraps over several lines", 32.0), None);
        assert!(h_big > h_one * 1.6, "bigger font is taller");
    }

    #[test]
    fn cache_reuses_shaping() {
        let mut t = TextSystem::new();
        let c = content("cached", 16.0);
        t.measure(&c, Some(200.0));
        let n = t.shape_count;
        t.measure(&c, Some(200.0));
        t.paragraph(&c, Some(200.0));
        assert_eq!(t.shape_count, n);
        t.measure(&c, Some(300.0));
        assert_eq!(t.shape_count, n, "a wider width reuses the unconstrained layout already shaped");
        t.measure(&c, Some(500.0));
        assert_eq!(t.shape_count, n);
        t.measure(&c, Some(20.0));
        assert_eq!(t.shape_count, n + 1, "a width that forces wrapping needs its own layout");
    }

    #[test]
    fn quantized_widths_share_shaping() {
        let mut t = TextSystem::new();
        let c = content("hello world this wraps over several lines when the width is narrow", 16.0);
        let (nw, _) = t.measure(&c, None);
        assert!(nw > 160.0, "test needs wrapping content, natural width {nw}");
        let n0 = t.shape_count;
        let (w1, h1) = t.measure(&c, Some(150.1));
        assert_eq!(t.shape_count, n0 + 1, "the unconstrained shape is already cached; one bucket shape");
        let (w2, h2) = t.measure(&c, Some(150.4));
        assert_eq!(t.shape_count, n0 + 1, "same 0.5px bucket shares the shaping");
        assert_eq!((w1, h1), (w2, h2));
        assert!(t.cache_hits >= 2, "bucket lookups hit: {}", t.cache_hits);
        // Wrap-correctness: the bucket paragraph was shaped at the bucket floor, so it fits.
        for w in [150.1f32, 150.4] {
            let p = t.paragraph(&c, Some(w));
            assert_eq!(p.wrap, Some(150.0));
            assert!(p.width <= w, "shaped width {} fits requested {w}", p.width);
            assert!(p.height > h1 * 0.99, "still wrapped");
        }
        t.measure(&c, Some(160.1));
        assert_eq!(t.shape_count, n0 + 2, "a new bucket shapes once more");
        assert!(t.hit_rate() > 0.4, "hit rate {}", t.hit_rate());
    }

    #[test]
    fn cache_evicts_lru_instead_of_clearing() {
        let mut t = TextSystem::new();
        let n = PARA_CACHE_CAP + 52;
        let mut last = content("placeholder", 16.0);
        for i in 0..n {
            last = content(&format!("eviction probe paragraph number {i}"), 16.0);
            t.measure(&last, Some(200.0));
        }
        assert_eq!(t.cache_len(), PARA_CACHE_CAP, "cache stays capped without a full clear");
        let shapes = t.shape_count;
        // The most recent content is still hot: no reshaping.
        t.measure(&last, Some(200.0));
        assert_eq!(t.shape_count, shapes, "recent entries survive; hits={}", t.cache_hits);
        // The earliest content was evicted long ago: reshapes instead of hitting a cleared cache.
        let first = content("eviction probe paragraph number 0", 16.0);
        t.measure(&first, Some(200.0));
        assert!(t.shape_count > shapes, "evicted entries reshape on demand");
        assert_eq!(t.cache_len(), PARA_CACHE_CAP);
    }

    #[test]
    fn empty_text_has_no_size() {
        let mut t = TextSystem::new();
        assert_eq!(t.measure(&content("", 16.0), None), (0.0, 0.0));
    }

    #[test]
    fn caret_and_hit_testing_are_consistent() {
        let mut t = TextSystem::new();
        let c = content("abcdef", 20.0);
        let (x0, _, h) = t.caret(&c, None, 0);
        let (x3, _, _) = t.caret(&c, None, 3);
        let (x6, _, _) = t.caret(&c, None, 6);
        assert!(x0 < 1.0 && x3 > x0 && x6 > x3 && h > 10.0, "{x0} {x3} {x6} {h}");
        assert_eq!(t.hit(&c, None, x3 + 1.0, 5.0), 3);
        assert_eq!(t.hit(&c, None, x6 + 50.0, 5.0), 6);
        let rects = t.selection_rects(&c, None, 1, 4);
        assert_eq!(rects.len(), 1);
        assert!(rects[0][0] > 0.0 && rects[0][2] > 0.0);
    }

    #[test]
    fn rasterizes_visible_glyphs() {
        let mut t = TextSystem::new();
        let p = t.paragraph(&content("Hi", 24.0), None);
        let g = &p.glyphs[0];
        let phys = g.glyph.physical((0.0, g.line_y), 1.0);
        let r = t.raster(phys.cache_key).expect("'H' has pixels");
        assert!(r.width > 4 && r.height > 8);
        assert!(r.rgba.chunks(4).any(|px| px[3] > 200), "has opaque coverage");
    }

    #[test]
    fn hard_breaks_make_lines() {
        let mut t = TextSystem::new();
        let s = Arc::new(ComputedStyle::default());
        let c = InlineContent {
            runs: vec![
                InlineRun { text: "a".into(), style: s.clone(), node: 1, is_break: false },
                InlineRun { text: String::new(), style: s.clone(), node: 1, is_break: true },
                InlineRun { text: "b".into(), style: s.clone(), node: 1, is_break: false },
            ],
            container: s,
        };
        let (_, h) = t.measure(&c, None);
        assert!(h >= 2.0 * 16.0 * 1.1, "two lines: {h}");
    }
}
