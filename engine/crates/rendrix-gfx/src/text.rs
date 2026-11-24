//! Text shaping and measurement on top of cosmic-text, with a paragraph cache.
//!
//! A [`Paragraph`] is the shaped, wrapped result for one `(content, width)` pair. Measurement during
//! layout and drawing during paint hit the same cache, so text is shaped once per change.

use cosmic_text::{Align, Attrs, Buffer, CacheKey, Family, FontSystem, Metrics, Shaping, Style, SwashCache, SwashContent, Weight, Wrap};
use cosmic_text::fontdb;
use rendrix_css::{FontStyle, TextAlign, TextTransform, WhiteSpace, WordBreak};
use rendrix_layout::{InlineContent, TextMeasurer};
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::sync::Arc;

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
    /// Number of visual lines produced by shaping (one per `layout_runs()` entry,
    /// including empty ones). Cached once at build time so the one-line reuse
    /// check in [`TextSystem::paragraph`] needs no per-lookup allocation/hash.
    pub line_count: usize,
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
///
/// Sized to hold a full first-frame key set: a ~1000-item list shapes ~10k distinct
/// `(content, width)` paragraphs during layout, so the old 2048 entry cap evicted
/// most of them before paint/rAF lookups could hit. The cache grows only to what an
/// app actually uses, so small apps still pay nothing.
pub const PARA_CACHE_CAP: usize = 8192;
/// Width quantisation step for the paragraph cache: widths falling in the same
/// step share one shaping. Shaping always uses the step's floor, so a cached
/// paragraph was wrapped at a width ≤ the requested one and can never overflow
/// it (at most it keeps one extra break near a bucket boundary).
pub const PARA_CACHE_QUANT: f32 = 0.5;

/// Minimum number of not-yet-shaped contents before [`TextSystem::prewarm_parallel`]
/// spawns worker threads. Below this, on-demand shaping beats thread-pool overhead.
pub const PREWARM_PARALLEL_MIN: usize = 16;

/// Compile-time proof that the prewarm pipeline can cross threads: the font database,
/// built paragraphs and inline contents are plain data (`Arc` contents are shared, not
/// owned by the workers). `FontSystem` itself is intentionally absent — it holds a
/// `Box<dyn Fallback>` and stays on its owning thread.
#[allow(dead_code)]
fn assert_prewarm_types_are_send() {
    fn is_send<T: Send>() {}
    is_send::<fontdb::Database>();
    is_send::<Paragraph>();
    is_send::<HashSet<String>>();
    is_send::<Arc<InlineContent>>();
}

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

/// Clone source for a content: the first fully shaped paragraph built for it. Every later
/// width variant (min-content, max-content, definite) re-wraps a clone of its `Buffer`
/// instead of re-running rustybuzz, which cosmic-text makes safe because `BufferLine`
/// clones carry the cached `ShapeLine` and `set_size` only re-runs the (cheap) wrapping.
struct TemplateEntry {
    para: Rc<Paragraph>,
    last_used: u64,
}

/// What the font-DB loader thread sends back to the owning `TextSystem`.
struct FontDbLoad {
    cache_hit: bool,
    locale: String,
    db: fontdb::Database,
    /// Loader-side duration, so the reported figure is the load itself, not the
    /// main-thread wait for it.
    elapsed_nanos: u64,
}

/// Font-DB loading state. `new()` (native) starts the load on a background thread so it
/// overlaps DOM parse + JS evaluation on the startup path; `ensure_loaded` joins it before
/// the first font use. Nothing outside `TextSystem` can observe the difference.
enum FontState {
    Loading(std::sync::mpsc::Receiver<FontDbLoad>),
    Loaded,
}

pub struct TextSystem {
    /// Private: every consumer goes through `ensure_loaded` so the background DB load
    /// cannot be bypassed. Same-crate tests use the `fonts()` accessor.
    fonts: FontSystem,
    font_state: FontState,
    swash: SwashCache,
    cache: HashMap<ParaKey, CacheEntry>,
    /// One fully shaped paragraph per content key (the rewrap clone source).
    templates: HashMap<u64, TemplateEntry>,
    clock: u64,
    installed: HashSet<String>,
    /// Stats for tests/profiling.
    /// Full rustybuzz shapes (one per content; later widths re-wrap instead).
    pub shape_count: usize,
    /// Paragraph builds served by cloning a template and re-wrapping (no rustybuzz).
    pub rewrap_count: usize,
    /// Paragraph-cache lookups served without shaping.
    pub cache_hits: usize,
    /// Cumulative nanoseconds spent in full rustybuzz shaping ([`TextSystem::build`]'s
    /// expensive path). Additive and monotonic; `rendrix-core` snapshots it per frame to
    /// attribute startup/first-frame cost to text shaping.
    pub shape_nanos: u64,
    /// Cumulative nanoseconds spent in template clone + re-wrap builds.
    pub rewrap_nanos: u64,
}

// Phase timers for the startup breakdown in `rendrix-runtime bench`. They live here because the
// font DB is opened inside `TextSystem::new`, before a `Session` or `Engine` exists. Thread-local
// so independent sessions/bench threads cannot contaminate each other's observation.
thread_local! {
    static FONT_DB_LOAD_NANOS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
    static FONT_DB_CACHE_HIT: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Nanoseconds spent loading/rebuilding the font database in the most recent `TextSystem::new()`.
pub fn last_font_db_load_nanos() -> u64 {
    FONT_DB_LOAD_NANOS.with(|c| c.get())
}

/// Whether the most recent `TextSystem::new()` was served by the on-disk font-DB cache.
pub fn last_font_db_cache_hit() -> bool {
    FONT_DB_CACHE_HIT.with(|c| c.get())
}

impl Default for TextSystem {
    fn default() -> Self {
        TextSystem::new()
    }
}

/// Exact replica of what `FontSystem::new()` does to the database on a cache miss, minus
/// the parts that need the `FontSystem` itself: scan system fonts, then install the same
/// generic-family defaults cosmic-text configures (and that `load_font_db_cache` mirrors
/// on the way back in). Building the `Database` off the main thread and the `FontSystem`
/// on it is bit-for-bit what `FontSystem::new()` would have produced.
fn scan_font_db() -> fontdb::Database {
    let mut db = fontdb::Database::new();
    db.load_system_fonts();
    db.set_monospace_family("Noto Sans Mono");
    db.set_sans_serif_family("Open Sans");
    db.set_serif_family("DejaVu Serif");
    db
}

/// Cache hit or full scan, in either case producing the same database `FontSystem::new()`
/// would have. Runs on the loader thread; the result is plain data and crosses back safely.
fn load_font_db(locale: &str) -> FontDbLoad {
    let start = std::time::Instant::now();
    if let Some(db) = load_font_db_cache(locale) {
        return FontDbLoad { cache_hit: true, locale: locale.to_string(), db, elapsed_nanos: start.elapsed().as_nanos() as u64 };
    }
    let db = scan_font_db();
    save_font_db_cache(locale, &db);
    FontDbLoad { cache_hit: false, locale: locale.to_string(), db, elapsed_nanos: start.elapsed().as_nanos() as u64 }
}

fn build_installed(fonts: &FontSystem) -> HashSet<String> {
    fonts
        .db()
        .faces()
        .flat_map(|f| f.families.iter().map(|(n, _)| n.to_lowercase()).collect::<Vec<_>>())
        .collect()
}

impl TextSystem {
    /// Web: browsers expose no system fonts to a worker, so ship a default family (Inter, SIL OFL,
    /// see `fonts/OFL.txt`) and map every generic family to it. Apps add their own with `@font-face`.
    /// Synchronous: workers have no thread spawn, and the embedded DB is already in memory.
    #[cfg(target_arch = "wasm32")]
    pub fn new() -> Self {
        let start = std::time::Instant::now();
        let mut db = fontdb::Database::new();
        db.load_font_data(include_bytes!("../fonts/Inter_400Regular.ttf").to_vec());
        db.load_font_data(include_bytes!("../fonts/Inter_700Bold.ttf").to_vec());
        db.set_sans_serif_family("Inter");
        db.set_serif_family("Inter");
        db.set_monospace_family("Inter");
        db.set_cursive_family("Inter");
        db.set_fantasy_family("Inter");
        FONT_DB_CACHE_HIT.with(|c| c.set(false));
        FONT_DB_LOAD_NANOS.with(|c| c.set(start.elapsed().as_nanos() as u64));
        let fonts = FontSystem::new_with_locale_and_db("en-US".to_string(), db);
        let installed = build_installed(&fonts);
        TextSystem { fonts, font_state: FontState::Loaded, swash: SwashCache::new(), cache: HashMap::new(), templates: HashMap::new(), clock: 0, installed, shape_count: 0, rewrap_count: 0, cache_hits: 0, shape_nanos: 0, rewrap_nanos: 0 }
    }

    /// Native: the font database loads on a background thread (disk cache hit or full
    /// system scan), overlapping DOM parse + JS evaluation on the startup path; the first
    /// font use joins it. A cache miss still pays the full scan once and populates the
    /// cache, exactly as before — the scan just happens while the rest of startup runs.
    /// `fontdb::Database` is plain data and crosses threads; `FontSystem` itself is not
    /// `Send`, so it is (re)built on the owning thread from the loaded database.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn new() -> Self {
        let locale = sys_locale::get_locale().unwrap_or_else(|| "en-US".to_string());
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::Builder::new()
            .name("rendrix-fontdb".into())
            .spawn(move || {
                let load = load_font_db(&locale);
                // A dropped receiver just means the TextSystem went away; the load is moot.
                let _ = tx.send(load);
            })
            .expect("font DB loader thread");
        // A placeholder font system: cheap to build (empty database), replaced by
        // `ensure_loaded` before anything can consume it.
        let fonts = FontSystem::new_with_locale_and_db(sys_locale::get_locale().unwrap_or_else(|| "en-US".to_string()), fontdb::Database::new());
        TextSystem { fonts, font_state: FontState::Loading(rx), swash: SwashCache::new(), cache: HashMap::new(), templates: HashMap::new(), clock: 0, installed: HashSet::new(), shape_count: 0, rewrap_count: 0, cache_hits: 0, shape_nanos: 0, rewrap_nanos: 0 }
    }

    /// Block until the background font-DB load (if any) finished and install the result.
    /// Idempotent; every `fonts`/`installed` consumer calls it first.
    fn ensure_loaded(&mut self) {
        let state = std::mem::replace(&mut self.font_state, FontState::Loaded);
        let FontState::Loading(rx) = state else { return };
        let load = match rx.recv() {
            Ok(load) => load,
            Err(_) => {
                // The loader thread died (never expected). Fall back to a synchronous
                // load so a font-DB failure can never take the engine down.
                let locale = sys_locale::get_locale().unwrap_or_else(|| "en-US".to_string());
                load_font_db(&locale)
            }
        };
        self.fonts = FontSystem::new_with_locale_and_db(load.locale, load.db);
        self.installed = build_installed(&self.fonts);
        FONT_DB_LOAD_NANOS.with(|c| c.set(load.elapsed_nanos));
        FONT_DB_CACHE_HIT.with(|c| c.set(load.cache_hit));
    }

    /// Font access for same-crate tests/tools; loads the DB first.
    pub fn fonts(&mut self) -> &mut FontSystem {
        self.ensure_loaded();
        &mut self.fonts
    }

    /// Number of paragraphs currently cached.
    pub fn cache_len(&self) -> usize {
        self.cache.len()
    }

    /// Hit rate over `paragraph()` lookups (`hits / (hits + shapes + rewraps)`).
    pub fn hit_rate(&self) -> f64 {
        let total = self.cache_hits + self.shape_count + self.rewrap_count;
        if total == 0 {
            1.0
        } else {
            self.cache_hits as f64 / total as f64
        }
    }

    /// Register an application font (TTF/OTF bytes).
    pub fn load_font(&mut self, data: Vec<u8>) {
        self.ensure_loaded();
        self.fonts.db_mut().load_font_data(data);
        self.installed = build_installed(&self.fonts);
        self.cache.clear();
        // New binary fonts can change every glyph: old templates are stale.
        self.templates.clear();
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

/// The concatenated (transformed) text of a content. Deterministic in `content`, so the
/// full-shape and rewrap paths derive the identical string.
fn content_text(content: &InlineContent) -> String {
    content
        .runs
        .iter()
        .map(|r| if r.is_break { "\n".to_string() } else { transform_text(&r.text, r.style.text_transform) })
        .collect()
}

/// Map a CSS font-family list onto a cosmic-text family, given the set of family names
/// installed in the font DB. Free-standing so worker threads (no `TextSystem`) resolve
/// families identically to the owning thread.
fn resolve_family_with<'a>(installed: &HashSet<String>, list: &'a [String]) -> Family<'a> {
    for name in list {
        match name.to_ascii_lowercase().as_str() {
            "sans-serif" | "system-ui" | "ui-sans-serif" | "-apple-system" | "blinkmacsystemfont" => return Family::SansSerif,
            "serif" | "ui-serif" => return Family::Serif,
            "monospace" | "ui-monospace" => return Family::Monospace,
            "cursive" => return Family::Cursive,
            "fantasy" => return Family::Fantasy,
            other => {
                if installed.contains(other) {
                    return Family::Name(name.as_str());
                }
            }
        }
    }
    Family::SansSerif
}

/// Wrap mode a content shapes with (from `white-space` / `word-break`).
fn content_wrap(content: &InlineContent) -> (bool, Wrap) {
    let c = &content.container;
    let nowrap = matches!(c.white_space, WhiteSpace::NoWrap | WhiteSpace::Pre);
    // `word-break` selects how cosmic-text may break within a word:
    //   normal    -> word boundaries, falling back to glyph breaks (CJK, long words)
    //   break-all -> any glyph may start a line, even for Latin scripts
    //   keep-all  -> never break inside a word (CJK runs stay intact)
    let wrap = if nowrap {
        Wrap::None
    } else {
        match c.word_break {
            WordBreak::BreakAll => Wrap::Glyph,
            WordBreak::KeepAll => Wrap::Word,
            WordBreak::Normal => Wrap::WordOrGlyph,
        }
    };
    (nowrap, wrap)
}

/// Full rustybuzz shape of `content` at `width`. The expensive path, taken once per
/// content; the result doubles as the rewrap template for every other width.
fn shape_paragraph(fonts: &mut FontSystem, installed: &HashSet<String>, content: &InlineContent, width: Option<f32>) -> Paragraph {
    let c = &content.container;
    let metrics = Metrics::new(c.font_size.max(1.0), c.line_height.resolve(c.font_size).max(1.0));
    let mut buffer = Buffer::new(fonts, metrics);
    let (nowrap, wrap) = content_wrap(content);
    buffer.set_wrap(fonts, wrap);
    buffer.set_size(fonts, if nowrap { None } else { width }, None);

    // Keep owned strings alive for the span borrow.
    let texts: Vec<String> = content
        .runs
        .iter()
        .map(|r| if r.is_break { "\n".to_string() } else { transform_text(&r.text, r.style.text_transform) })
        .collect();
    let families: Vec<Family> = content.runs.iter().map(|r| resolve_family_with(installed, &r.style.font_family)).collect();
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
    buffer.set_rich_text(fonts, spans, &default_attrs, Shaping::Advanced, Some(align));
    buffer.shape_until_scroll(fonts, false);
    finish_paragraph(buffer, &texts.concat(), width)
}

/// Re-wrap an already-shaped template buffer at another width. The clone carries the
/// cached `ShapeLine` (the rustybuzz output), so `set_size` re-runs only cosmic-text's
/// wrapping over the same glyphs — producing exactly what `shape_paragraph` would.
fn rewrap_paragraph(template: &Buffer, fonts: &mut FontSystem, content: &InlineContent, width: Option<f32>) -> Paragraph {
    let mut buffer = template.clone();
    let (nowrap, _wrap) = content_wrap(content);
    // `set_size` triggers relayout + shaping (a no-op over the cached `ShapeLine`).
    buffer.set_size(fonts, if nowrap { None } else { width }, None);
    buffer.shape_until_scroll(fonts, false);
    finish_paragraph(buffer, &content_text(content), width)
}

/// Shared post-processing: walk the laid-out buffer into the engine's paragraph
/// representation. Identical for the full-shape and rewrap paths, which is what makes
/// the two measurement-identical.
fn finish_paragraph(buffer: Buffer, text: &str, width: Option<f32>) -> Paragraph {
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
    let mut line_count = 0usize;
    for run in buffer.layout_runs() {
        line_count += 1;
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
    Paragraph { width: max_w, height, line_count, glyphs, segments, text: text.to_string(), buffer, line_starts, wrap: width }
}

impl TextSystem {

    /// Build the paragraph for `(content, width)`, full-shaping at most once per content.
    ///
    /// The first build for a content runs the real rustybuzz pipeline and registers the
    /// result as that content's *template*. Every later width for the same content clones
    /// the template's [`Buffer`] and re-wraps it: cosmic-text keeps the shaped
    /// `ShapeLine` inside a cloned `BufferLine`, and `set_size` only invalidates the
    /// (cheap) wrapping, so the rewrap is bit-for-bit what a fresh full shape would have
    /// produced — pinned by the `rewrap_is_measurement_identical` test — at a fraction of
    /// the cost. `shape_count` counts full shapes; `rewrap_count` counts clone+rewraps.
    fn build(&mut self, content: &InlineContent, width: Option<f32>) -> Rc<Paragraph> {
        self.ensure_loaded();
        let key = content.key();
        if let Some(t) = self.templates.get_mut(&key) {
            self.clock = self.clock.wrapping_add(1);
            t.last_used = self.clock;
            let t0 = std::time::Instant::now();
            let para = Rc::new(rewrap_paragraph(&t.para.buffer, &mut self.fonts, content, width));
            self.rewrap_count += 1;
            self.rewrap_nanos += t0.elapsed().as_nanos() as u64;
            return para;
        }
        let t0 = std::time::Instant::now();
        let para = Rc::new(shape_paragraph(&mut self.fonts, &self.installed, content, width));
        self.shape_count += 1;
        self.shape_nanos += t0.elapsed().as_nanos() as u64;
        // LRU-bounded: same cap as the paragraph cache, so the template set (which pins
        // its paragraphs' shaped buffers) can never grow unbounded.
        if self.templates.len() >= PARA_CACHE_CAP {
            if let Some(victim) = self.templates.iter().min_by_key(|(_, e)| e.last_used).map(|(k, _)| *k) {
                self.templates.remove(&victim);
            }
        }
        self.clock = self.clock.wrapping_add(1);
        self.templates.insert(key, TemplateEntry { para: para.clone(), last_used: self.clock });
        para
    }

    /// Shape a batch of `(content, width)` pairs into the paragraph cache *before* the first frame.
    ///
    /// This is the safe lever for moving cold-start shaping off the critical first-frame path: an app
    /// that knows its initial content (e.g. a static first screen, or a list built by a loading script)
    /// can call this during a loading phase, then the first frame's `measure`/paint calls are cache
    /// hits. Each item goes through [`TextSystem::paragraph`], so prewarmed entries are exactly the
    /// ones on-demand shaping would have produced — no separate code path, hence no identity risk.
    ///
    /// Intentionally opt-in: the engine cannot know future content, and forcing a prewarm by walking
    /// the DOM would only move the work into a different bucket of the same startup number. Callers
    /// with a genuine idle window should use it; everyone else pays nothing.
    pub fn prewarm<'a>(&mut self, items: impl IntoIterator<Item = (&'a InlineContent, Option<f32>)>) {
        for (content, width) in items {
            let _ = self.paragraph(content, width);
        }
    }

    /// First-frame shaping across worker threads.
    ///
    /// [`prewarm`](Self::prewarm) is the general opt-in lever; this is the cold-start
    /// specialisation. A first frame measures every text block at min-content *and*
    /// max-content before painting, so for a large initial DOM almost all of its shaping
    /// is knowable the moment the box tree exists. The contents are chunked across
    /// short-lived worker threads, each with its own `FontSystem` built from a clone of
    /// this system's font database (deterministic: same DB, same cosmic-text, same
    /// glyphs). Each worker full-shapes its contents once (unconstrained width, which
    /// doubles as the rewrap template) and re-wraps the min-content variant from it.
    /// Results are inserted through the same cache the on-demand path uses, keyed exactly
    /// as [`paragraph`](Self::paragraph) would key them, so the first frame's layout
    /// lookups hit instead of shaping. Below [`PREWARM_PARALLEL_MIN`] fresh contents the
    /// call is a no-op: on-demand shaping is cheaper than a thread pool for small apps.
    ///
    /// Workers that fail (panic, OOM) are simply skipped: their contents shape on demand,
    /// so a prewarm failure can only cost time, never correctness.
    pub fn prewarm_parallel(&mut self, contents: &[Arc<InlineContent>]) {
        if contents.is_empty() {
            return;
        }
        // Dedup by content key; skip anything already shaped (its template exists).
        let mut seen = std::collections::HashSet::new();
        let fresh: Vec<Arc<InlineContent>> = contents
            .iter()
            .filter(|c| seen.insert(c.key()) && !self.templates.contains_key(&c.key()))
            .cloned()
            .collect();
        if fresh.len() < PREWARM_PARALLEL_MIN {
            return;
        }
        self.prewarm_parallel_impl(&fresh);
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn prewarm_parallel_impl(&mut self, fresh: &[Arc<InlineContent>]) {
        self.ensure_loaded();
        let threads = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4).clamp(2, 8).min(fresh.len());
        let locale = self.fonts.locale().to_string();
        let db = self.fonts.db().clone();
        let installed = self.installed.clone();
        let chunk = (fresh.len() + threads - 1) / threads;
        let mut shape_nanos = 0u64;
        let mut rewrap_nanos = 0u64;
        std::thread::scope(|scope| {
            let mut handles = Vec::with_capacity(threads);
            for part in fresh.chunks(chunk) {
                let (locale, db, installed) = (locale.clone(), db.clone(), installed.clone());
                handles.push(scope.spawn(move || {
                    let mut fonts = FontSystem::new_with_locale_and_db(locale, db);
                    let mut out = Vec::with_capacity(part.len());
                    let (mut full_ns, mut rewrap_ns) = (0u64, 0u64);
                    for content in part {
                        let key = content.key();
                        let t0 = std::time::Instant::now();
                        let natural = shape_paragraph(&mut fonts, &installed, content, None);
                        full_ns += t0.elapsed().as_nanos() as u64;
                        // Min-content variant, the other half of every first-frame
                        // measurement pair. `nowrap` content normalises every width to
                        // None, so the unconstrained shape above already covers it.
                        let zero = if content_wrap(content).0 { None } else {
                            let t1 = std::time::Instant::now();
                            let p = rewrap_paragraph(&natural.buffer, &mut fonts, content, Some(0.0));
                            rewrap_ns += t1.elapsed().as_nanos() as u64;
                            Some(p)
                        };
                        out.push((key, natural, zero));
                    }
                    (out, full_ns, rewrap_ns)
                }));
            }
            for handle in handles {
                let Ok((built, full_ns, rewrap_ns)) = handle.join() else { continue };
                shape_nanos += full_ns;
                rewrap_nanos += rewrap_ns;
                for (key, natural, zero) in built {
                    self.shape_count += 1;
                    if zero.is_some() {
                        self.rewrap_count += 1;
                    }
                    self.insert_prewarmed(key, natural, zero);
                }
            }
        });
        self.shape_nanos += shape_nanos;
        self.rewrap_nanos += rewrap_nanos;
    }

    #[cfg(target_arch = "wasm32")]
    fn prewarm_parallel_impl(&mut self, fresh: &[Arc<InlineContent>]) {
        // No thread spawn on the web target: shape serially through the normal path,
        // which still leaves the first frame with cache hits.
        for content in fresh {
            let _ = self.paragraph(content, None);
        }
    }

    /// Cache a worker-built paragraph pair under the exact keys the on-demand path uses
    /// (None → width_bits 0, Some(0.0) → width_bits 1), registering the unconstrained
    /// paragraph as the content's rewrap template. Mirrors `paragraph`'s miss path,
    /// including LRU eviction, so prewarmed and on-demand entries are indistinguishable.
    fn insert_prewarmed(&mut self, key: u64, natural: Paragraph, zero: Option<Paragraph>) {
        let natural = Rc::new(natural);
        self.clock = self.clock.wrapping_add(1);
        self.insert_bounded(ParaKey { content: key, width_bits: 0 }, natural.clone(), self.clock);
        if let Some(zero) = zero {
            self.clock = self.clock.wrapping_add(1);
            self.insert_bounded(ParaKey { content: key, width_bits: width_bits(0.0) }, Rc::new(zero), self.clock);
        }
        if self.templates.len() >= PARA_CACHE_CAP {
            if let Some(victim) = self.templates.iter().min_by_key(|(_, e)| e.last_used).map(|(k, _)| *k) {
                self.templates.remove(&victim);
            }
        }
        self.clock = self.clock.wrapping_add(1);
        self.templates.insert(key, TemplateEntry { para: natural, last_used: self.clock });
    }

    /// Insert with the same LRU bound as `paragraph`'s miss path.
    fn insert_bounded(&mut self, key: ParaKey, para: Rc<Paragraph>, last_used: u64) {
        if self.cache.len() >= PARA_CACHE_CAP && !self.cache.contains_key(&key) {
            if let Some(victim) = self.cache.iter().min_by_key(|(_, e)| e.last_used).map(|(k, _)| *k) {
                self.cache.remove(&victim);
            }
        }
        self.cache.insert(key, CacheEntry { para, last_used });
    }

    pub fn paragraph(&mut self, content: &InlineContent, width: Option<f32>) -> Rc<Paragraph> {
        // `build()` ignores the width for nowrap/pre content (Wrap::None plus an
        // unconstrained size), so every width yields the same paragraph. Normalize
        // before keying so layout's min-content (Some(0)), max-content (None) and
        // definite-width passes share one shaping instead of three.
        let width = if matches!(content.container.white_space, WhiteSpace::NoWrap | WhiteSpace::Pre) {
            None
        } else {
            width
        };
        // When the text fits on one line and is left-aligned, the unconstrained layout is identical at
        // any wider width: share it instead of shaping again.
        if let Some(w) = width {
            if matches!(content.container.text_align, TextAlign::Left | TextAlign::Start) {
                let natural = self.paragraph(content, None);
                if natural.width <= w && natural.line_count <= 1 {
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
        let p = self.build(content, qw);
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
        self.ensure_loaded();
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

// ---- Font DB disk cache (R1.3 cold start) -----------------------------------------
//
// `FontSystem::new()` scans and fully parses every installed system font, which
// dominates cold start (~0.75s). The parsed metadata is small and stable, so it is
// cached on disk keyed by (OS, arch, font-backend versions, locale). A cache hit
// rebuilds the `fontdb::Database` via `push_face_info` without reading any font
// file's contents; each entry is validated against its file's size + mtime, and any
// mismatch, corruption or I/O error falls back to a full scan (never a wrong DB).
// Binary (app-loaded) fonts are not cached: they are re-registered through
// `load_font`, which clears the paragraph cache as before.
//
// Shaping stays lazy: paragraphs are shaped on first use through the paragraph
// cache below, so offscreen paragraphs cost nothing until they are measured or
// painted (paint additionally skips fully-clipped text before shaping; see the
// renderer), and the font DB itself is reused across runs via this cache.
//
// Why the engine does NOT share one `FontSystem` across threads (considered and
// rejected): every shaping call needs `&mut FontSystem`, `TextSystem` is reached through
// a `RefCell` and hands out `Rc<Paragraph>` (not `Send`), and sharing would need a lock
// held for the whole shape, serialising anyway. The cold-start levers instead:
// `TextSystem::prewarm` (batch known content serially), template rewraps (clone a shaped
// buffer, re-wrap at another width — no rustybuzz), and `prewarm_parallel` (first-frame
// contents chunked over short-lived worker threads, each with its OWN `FontSystem` built
// from a clone of this system's `fontdb::Database`; same DB + same cosmic-text ⇒
// bit-identical glyphs, inserted through the ordinary cache keys afterwards).
//
// Env overrides (tests/debugging):
// - `RENDRIX_CACHE_DIR`: directory holding `fontdb-v1.cache`.
// - `RENDRIX_NO_FONT_CACHE`: skip both load and save.

const FONT_CACHE_MAGIC: u32 = 0x52445846; // "RDXF"
const FONT_CACHE_VERSION: u32 = 1;

/// Cache key: OS + arch + the font-backend versions that fix the serialized schema.
/// Bump `FONT_CACHE_VERSION` (or extend this) when upgrading cosmic-text/fontdb/ttf-parser.
fn font_cache_key() -> String {
    format!(
        "rendrix-fontdb-v1|{}|{}|cosmic-0.14.2|fontdb-0.16.2|ttf-lang-0.20",
        std::env::consts::OS,
        std::env::consts::ARCH
    )
}

fn font_cache_path() -> std::path::PathBuf {
    use std::path::PathBuf;
    if let Some(dir) = std::env::var_os("RENDRIX_CACHE_DIR") {
        return PathBuf::from(dir).join("fontdb-v1.cache");
    }
    let base: Option<PathBuf> = {
        #[cfg(target_os = "windows")]
        {
            std::env::var_os("LOCALAPPDATA").map(PathBuf::from)
        }
        #[cfg(target_os = "macos")]
        {
            std::env::var_os("HOME").map(|h| PathBuf::from(h).join("Library/Caches"))
        }
        #[cfg(not(any(target_os = "windows", target_os = "macos")))]
        {
            std::env::var_os("XDG_CACHE_HOME")
                .map(PathBuf::from)
                .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache")))
        }
    };
    base.unwrap_or_else(std::env::temp_dir).join("rendrix").join("fontdb-v1.cache")
}

fn cache_write_u16(out: &mut Vec<u8>, x: u16) {
    out.extend_from_slice(&x.to_le_bytes());
}

fn cache_write_u32(out: &mut Vec<u8>, x: u32) {
    out.extend_from_slice(&x.to_le_bytes());
}

fn cache_write_u64(out: &mut Vec<u8>, x: u64) {
    out.extend_from_slice(&x.to_le_bytes());
}

fn cache_write_str(out: &mut Vec<u8>, s: &str) {
    cache_write_u32(out, s.len() as u32);
    out.extend_from_slice(s.as_bytes());
}

struct CacheReader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> CacheReader<'a> {
    fn bytes(&mut self, n: usize) -> Option<&'a [u8]> {
        let end = self.pos.checked_add(n)?;
        let s = self.data.get(self.pos..end)?;
        self.pos = end;
        Some(s)
    }

    fn u8(&mut self) -> Option<u8> {
        Some(*self.bytes(1)?.first()?)
    }

    fn u16(&mut self) -> Option<u16> {
        let b: [u8; 2] = self.bytes(2)?.try_into().ok()?;
        Some(u16::from_le_bytes(b))
    }

    fn u32(&mut self) -> Option<u32> {
        let b: [u8; 4] = self.bytes(4)?.try_into().ok()?;
        Some(u32::from_le_bytes(b))
    }

    fn u64(&mut self) -> Option<u64> {
        let b: [u8; 8] = self.bytes(8)?.try_into().ok()?;
        Some(u64::from_le_bytes(b))
    }

    fn str(&mut self) -> Option<String> {
        let n = self.u32()? as usize;
        if n > 1 << 20 {
            return None;
        }
        Some(std::str::from_utf8(self.bytes(n)?).ok()?.to_string())
    }
}

/// Map a serialized `fontdb::Language` discriminant back to the enum.
/// fontdb 0.16.2 (ttf-parser 0.20) has 205 variants with discriminants 0..=204;
/// the cache key pins the backend versions, so any other value means corruption.
/// NOTE: this is the *fontdb* `Language`, not `cosmic_text::ttf_parser::Language`
/// (different ttf-parser versions in the tree): `FaceInfo.families` needs the former.
fn language_from_disc(d: u32) -> Option<fontdb::Language> {
    if d <= 204 {
        // Sound: fieldless enum with 1-byte layout, `d` is a valid discriminant
        // in this fontdb/ttf-parser (a wrong bound fails to compile via E0512).
        Some(unsafe { std::mem::transmute::<u8, fontdb::Language>(d as u8) })
    } else {
        None
    }
}

fn width_from_number(n: u16) -> Option<fontdb::Stretch> {
    use fontdb::Stretch as W;
    match n {
        1 => Some(W::UltraCondensed),
        2 => Some(W::ExtraCondensed),
        3 => Some(W::Condensed),
        4 => Some(W::SemiCondensed),
        5 => Some(W::Normal),
        6 => Some(W::SemiExpanded),
        7 => Some(W::Expanded),
        8 => Some(W::ExtraExpanded),
        9 => Some(W::UltraExpanded),
        _ => None,
    }
}

/// Serialize the system-font metadata of `db` to the cache path (atomic write).
/// File-backed faces only; in-memory faces are re-registered via `load_font`.
fn save_font_db_cache(locale: &str, db: &fontdb::Database) {
    if std::env::var_os("RENDRIX_NO_FONT_CACHE").is_some() {
        return;
    }
    let mut out = Vec::with_capacity(64 * 1024);
    cache_write_u32(&mut out, FONT_CACHE_MAGIC);
    cache_write_u32(&mut out, FONT_CACHE_VERSION);
    cache_write_str(&mut out, &font_cache_key());
    cache_write_str(&mut out, locale);
    // Collect first so an empty DB never clobbers a good cache.
    let mut faces: Vec<(String, &fontdb::FaceInfo)> = Vec::new();
    for face in db.faces() {
        if let fontdb::Source::File(path) = &face.source {
            if let Some(s) = path.to_str() {
                faces.push((s.to_string(), face));
            }
        }
    }
    if faces.is_empty() {
        return;
    }
    cache_write_u32(&mut out, faces.len() as u32);
    for (path_str, face) in &faces {
        let meta = match std::fs::metadata(path_str) {
            Ok(m) => m,
            Err(_) => return,
        };
        let mtime = match meta.modified().ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()) {
            Some(t) => t,
            None => return,
        };
        cache_write_str(&mut out, path_str);
        cache_write_u32(&mut out, face.index);
        cache_write_u64(&mut out, meta.len());
        cache_write_u64(&mut out, mtime.as_secs());
        cache_write_u32(&mut out, mtime.subsec_nanos());
        cache_write_u32(&mut out, face.families.len() as u32);
        for (name, lang) in &face.families {
            cache_write_str(&mut out, name);
            cache_write_u32(&mut out, *lang as u32);
        }
        cache_write_str(&mut out, &face.post_script_name);
        cache_write_u32(&mut out, face.style as u32);
        cache_write_u32(&mut out, face.weight.0 as u32);
        cache_write_u16(&mut out, face.stretch.to_number());
        out.push(face.monospaced as u8);
    }
    let path = font_cache_path();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    // Atomic publish: readers never see a half-written cache.
    let tmp = path.with_extension("tmp");
    if std::fs::write(&tmp, &out).is_ok() {
        let _ = std::fs::rename(&tmp, &path);
    }
}

/// Rebuild a `fontdb::Database` from the cache path. `None` on any miss,
/// corruption, font change (size/mtime) or key mismatch: the caller falls back
/// to a full system scan.
fn load_font_db_cache(locale: &str) -> Option<fontdb::Database> {
    if std::env::var_os("RENDRIX_NO_FONT_CACHE").is_some() {
        return None;
    }
    let data = std::fs::read(font_cache_path()).ok()?;
    let mut r = CacheReader { data: &data, pos: 0 };
    if r.u32()? != FONT_CACHE_MAGIC || r.u32()? != FONT_CACHE_VERSION {
        return None;
    }
    if r.str()? != font_cache_key() || r.str()? != locale {
        return None;
    }
    let n = r.u32()?;
    if n == 0 || n > 100_000 {
        return None;
    }
    let mut db = fontdb::Database::new();
    // Same generic-family defaults cosmic-text installs in `FontSystem::new_with_fonts`.
    db.set_monospace_family("Noto Sans Mono");
    db.set_sans_serif_family("Open Sans");
    db.set_serif_family("DejaVu Serif");
    for _ in 0..n {
        let path_str = r.str()?;
        let index = r.u32()?;
        let size = r.u64()?;
        let sec = r.u64()?;
        let nanos = r.u32()?;
        let meta = std::fs::metadata(&path_str).ok()?;
        if meta.len() != size {
            return None;
        }
        let mtime = meta.modified().ok()?.duration_since(std::time::UNIX_EPOCH).ok()?;
        if mtime.as_secs() != sec || mtime.subsec_nanos() != nanos {
            return None;
        }
        let nfam = r.u32()?;
        if nfam == 0 || nfam > 64 {
            return None;
        }
        let mut families = Vec::with_capacity(nfam as usize);
        for _ in 0..nfam {
            let name = r.str()?;
            let lang = language_from_disc(r.u32()?)?;
            families.push((name, lang));
        }
        let post_script_name = r.str()?;
        let style = match r.u32()? {
            0 => fontdb::Style::Normal,
            1 => fontdb::Style::Italic,
            2 => fontdb::Style::Oblique,
            _ => return None,
        };
        let weight = fontdb::Weight(r.u32()? as u16);
        let stretch = width_from_number(r.u16()?)?;
        let monospaced = match r.u8()? {
            0 => false,
            1 => true,
            _ => return None,
        };
        // `push_face_info` stores the pre-parsed metadata without touching the
        // font file: this is the whole cold-start saving.
        db.push_face_info(fontdb::FaceInfo {
            id: fontdb::ID::dummy(),
            source: fontdb::Source::File(path_str.into()),
            index,
            families,
            post_script_name,
            style,
            weight,
            stretch,
            monospaced,
        });
    }
    if r.pos != data.len() {
        return None;
    }
    Some(db)
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
        InlineContent::new(vec![InlineRun { text: text.into(), style: s.clone(), node: 1, is_break: false }], s)
    }

    /// A stable signature of everything measurement/paint consumes from a paragraph. Hashing f32
    /// bits (not `==`) makes even a sub-ULP change in glyph position fail the assertion, so an
    /// optimization cannot silently change rendered geometry while "still looking about right".
    fn snapshot(p: &Paragraph) -> String {
        let mut s = format!("{}|{}|{}|{}|", p.width.to_bits(), p.height.to_bits(), p.line_count, p.text);
        for g in &p.glyphs {
            s.push_str(&format!("g:{}/{}/{}/{}/{};", g.glyph.x.to_bits(), g.glyph.w.to_bits(), g.glyph.start, g.glyph.end, g.glyph.metadata));
        }
        for seg in &p.segments {
            s.push_str(&format!("s:{}/{}/{}/{}/{};", seg.run, seg.x.to_bits(), seg.w.to_bits(), seg.line_y.to_bits(), seg.line_height.to_bits()));
        }
        s
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
        let n = t.shape_count + t.rewrap_count;
        t.measure(&c, Some(200.0));
        t.paragraph(&c, Some(200.0));
        assert_eq!(t.shape_count + t.rewrap_count, n);
        t.measure(&c, Some(300.0));
        assert_eq!(t.shape_count + t.rewrap_count, n, "a wider width reuses the unconstrained layout already shaped");
        t.measure(&c, Some(500.0));
        assert_eq!(t.shape_count + t.rewrap_count, n);
        t.measure(&c, Some(20.0));
        // A width that forces wrapping gets its own layout; with a template already
        // shaped, that build is a clone+rewrap rather than a second rustybuzz shape.
        assert_eq!(t.shape_count + t.rewrap_count, n + 1, "a width that forces wrapping needs its own layout");
        assert_eq!(t.rewrap_count, 1, "the wrapped variant is built by template rewrap");
    }

    #[test]
    fn nowrap_widths_share_one_shaping() {
        let mut t = TextSystem::new();
        let mut s = ComputedStyle::default();
        s.font_size = 16.0;
        s.white_space = WhiteSpace::NoWrap;
        let s = Arc::new(s);
        let c = InlineContent::new(
            vec![InlineRun { text: "nowrap text that would wrap if allowed".into(), style: s.clone(), node: 1, is_break: false }],
            s,
        );
        let shapes = t.shape_count;
        let reference = snapshot(&t.paragraph(&c, None));
        // Layout asks for min-content (0), max-content (None) and a definite width;
        // none of them can change a nowrap paragraph's geometry.
        for w in [Some(0.0), Some(120.0), Some(600.0)] {
            let p = t.paragraph(&c, w);
            assert_eq!(snapshot(&p), reference, "nowrap geometry must not depend on width {w:?}");
            assert_eq!(p.wrap, None, "nowrap paragraphs carry no wrapping constraint");
        }
        assert_eq!(t.shape_count, shapes + 1, "all widths share a single shaping");
    }

    #[test]
    fn quantized_widths_share_shaping() {
        let mut t = TextSystem::new();
        let c = content("hello world this wraps over several lines when the width is narrow", 16.0);
        let (nw, _) = t.measure(&c, None);
        assert!(nw > 160.0, "test needs wrapping content, natural width {nw}");
        // `build`s = full shapes + template rewraps (a rewrap serves a new bucket once
        // the content has a shaped template; either way, one build per bucket).
        let n0 = t.shape_count + t.rewrap_count;
        let (w1, h1) = t.measure(&c, Some(150.1));
        assert_eq!(t.shape_count + t.rewrap_count, n0 + 1, "the unconstrained shape is already cached; one bucket build");
        let (w2, h2) = t.measure(&c, Some(150.4));
        assert_eq!(t.shape_count + t.rewrap_count, n0 + 1, "same 0.5px bucket shares the shaping");
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
        assert_eq!(t.shape_count + t.rewrap_count, n0 + 2, "a new bucket builds once more");
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
        let c = InlineContent::new(
            vec![
                InlineRun { text: "a".into(), style: s.clone(), node: 1, is_break: false },
                InlineRun { text: String::new(), style: s.clone(), node: 1, is_break: true },
                InlineRun { text: "b".into(), style: s.clone(), node: 1, is_break: false },
            ],
            s,
        );
        let (_, h) = t.measure(&c, None);
        assert!(h >= 2.0 * 16.0 * 1.1, "two lines: {h}");
    }

    #[test]
    fn paragraph_records_line_count() {
        let mut t = TextSystem::new();
        let one = t.paragraph(&content("a single line", 16.0), None);
        assert_eq!(one.line_count, 1, "one visual line");
        let two = t.paragraph(&content("first line\nsecond line", 16.0), None);
        assert_eq!(two.line_count, 2, "two newline-separated visual lines");
    }

    // ---- Cache-correctness / measurement-identity guards ---------------------------------------
    // These pin the *result* of shaping (glyph geometry, not just counts), so any future
    // prewarm/batching/parallelization cannot change what is rendered without failing here.

    /// Contents exercising the paths that rewrapping must survive: wrapping, single words,
    /// spaces, CJK (glyph wrap fallback), hard breaks, uppercase transform.
    fn identity_corpus() -> Vec<InlineContent> {
        let mut out = vec![
            content("Item number 5 with some descriptive text", 14.0),
            content("hello world with several words to wrap around a narrow column", 16.0),
            content("supercalifragilisticexpialidocious", 12.0),
            content("  leading and trailing spaces  ", 16.0),
            content("mixed script 日本語 and latin text", 15.0),
            content("", 16.0),
        ];
        // Hard break between two runs.
        let s = Arc::new(ComputedStyle::default());
        out.push(InlineContent::new(
            vec![
                InlineRun { text: "first line".into(), style: s.clone(), node: 1, is_break: false },
                InlineRun { text: String::new(), style: s.clone(), node: 1, is_break: true },
                InlineRun { text: "second line".into(), style: s.clone(), node: 1, is_break: false },
            ],
            s.clone(),
        ));
        // Two runs with different font sizes (separate cosmic spans).
        let big = ComputedStyle { font_size: 24.0, ..Default::default() };
        out.push(InlineContent::new(
            vec![
                InlineRun { text: "small ".into(), style: s.clone(), node: 1, is_break: false },
                InlineRun { text: "BIG".into(), style: Arc::new(big), node: 1, is_break: false },
            ],
            s.clone(),
        ));
        out
    }

    /// A width for each corpus entry that forces wrapping (so rewrap and full shape
    /// actually produce multi-line layouts).
    fn narrow_widths() -> Vec<Option<f32>> {
        vec![
            Some(60.0), Some(80.0), Some(30.0), Some(40.0), Some(60.0), Some(10.0),
            Some(30.0), Some(40.0),
        ]
    }

    #[test]
    fn rewrap_is_measurement_identical() {
        // System `fresh` full-shapes every width from scratch; system `rewrapped`
        // builds one width first (creating the template) and then serves the same
        // width by clone+rewrap. The snapshots must agree bit for bit.
        for (i, c) in identity_corpus().iter().enumerate() {
            for w in [narrow_widths()[i], Some(0.0), None] {
                let mut fresh = TextSystem::new();
                let direct = snapshot(&fresh.paragraph(c, w));
                let mut rewrapped = TextSystem::new();
                // First build a different width: this creates the template.
                let _ = rewrapped.paragraph(c, if w == Some(0.0) { Some(137.0) } else { Some(0.0) });
                let via_template = snapshot(&rewrapped.paragraph(c, w));
                assert_eq!(via_template, direct, "corpus[{i}] at width {w:?}: rewrap == fresh full shape");
            }
        }
    }

    #[test]
    fn prewarm_parallel_matches_on_demand_and_leaves_hits() {
        let mut corpus = identity_corpus();
        // The parallel path engages only above its threshold; pad with unique fillers.
        for i in 0..PREWARM_PARALLEL_MIN {
            corpus.push(content(&format!("filler paragraph {i} with a few wrapped words"), 14.0));
        }
        // On-demand baseline: every paragraph shaped through the normal path.
        let mut on_demand = TextSystem::new();
        let expected: Vec<String> = corpus.iter().map(|c| snapshot(&on_demand.paragraph(c, None))).collect();

        // Prewarmed system: workers shape, the cache serves afterwards.
        let mut prewarmed = TextSystem::new();
        let arcs: Vec<Arc<InlineContent>> = corpus.iter().map(|c| Arc::new(c.clone())).collect();
        prewarmed.prewarm_parallel(&arcs);
        for (c, want) in corpus.iter().zip(&expected) {
            assert_eq!(&snapshot(&prewarmed.paragraph(c, None)), want, "prewarmed paragraph == on-demand paragraph");
        }
        // Post-prewarm lookups must be pure cache hits.
        let (shapes, rewraps, hits) = (prewarmed.shape_count, prewarmed.rewrap_count, prewarmed.cache_hits);
        for c in &corpus {
            let _ = prewarmed.paragraph(c, None);
        }
        assert_eq!(prewarmed.shape_count, shapes, "post-prewarm lookup does not full-shape");
        assert_eq!(prewarmed.rewrap_count, rewraps, "post-prewarm lookup does not rewrap");
        assert!(prewarmed.cache_hits > hits, "post-prewarm lookup is a cache hit");
    }

    #[test]
    fn memoized_shape_is_measurement_identical() {
        let mut t = TextSystem::new();
        let c = content("the quick brown fox jumps over the lazy dog", 17.0);
        let first = snapshot(&t.paragraph(&c, Some(180.0)));
        let shapes = t.shape_count;
        // Several repeat lookups (including via `measure`) must all serve the cached paragraph.
        for _ in 0..5 {
            assert_eq!(snapshot(&t.paragraph(&c, Some(180.0))), first);
            assert!(t.measure(&c, Some(180.0)).0 > 0.0);
        }
        assert_eq!(t.shape_count, shapes, "repeats never re-shape");
        assert_eq!(snapshot(&t.paragraph(&c, Some(180.0))), first, "cache hit == fresh shape");
    }

    #[test]
    fn width_bucket_hit_matches_fresh_shape() {
        let mut t = TextSystem::new();
        let c = content("a wrapped sentence that needs more than one line at this width", 16.0);
        // 150.1 and 150.4 land in the same 0.5px bucket (floor 150.0).
        assert_eq!(quantize_width(150.1), quantize_width(150.4));
        assert_eq!(width_bits(quantize_width(150.1)), width_bits(quantize_width(150.4)));
        let at_floor = {
            let mut fresh = TextSystem::new();
            let p = fresh.paragraph(&c, Some(150.0));
            snapshot(&p)
        };
        let at_150_1 = snapshot(&t.paragraph(&c, Some(150.1)));
        let at_150_4 = snapshot(&t.paragraph(&c, Some(150.4)));
        assert_eq!(at_150_1, at_150_4, "both widths in a bucket share one paragraph object");
        assert_eq!(at_150_1, at_floor, "bucket shape == a directly-shaped bucket floor");
        // Guard the quantiser's defensive edges (non-finite / negative → zero bucket).
        assert_eq!(quantize_width(f32::NAN), 0.0);
        assert_eq!(quantize_width(f32::INFINITY), 0.0);
        assert_eq!(quantize_width(-12.0), 0.0);
    }

    #[test]
    fn eviction_then_reshape_is_measurement_identical() {
        let mut t = TextSystem::new();
        let c = content("identity survives an LRU eviction: measure me twice", 16.0);
        let before = snapshot(&t.paragraph(&c, Some(220.0)));
        // Force the original entry out of the paragraph cache. The content's template
        // survives, so the rebuild is a clone+rewrap (still one new paragraph).
        for i in 0..(PARA_CACHE_CAP + 8) {
            let other = content(&format!("filler paragraph {i} with enough words to shape"), 16.0);
            t.measure(&other, Some(220.0));
        }
        assert!(t.cache_len() <= PARA_CACHE_CAP);
        let (shapes, rewraps) = (t.shape_count, t.rewrap_count);
        let after = snapshot(&t.paragraph(&c, Some(220.0)));
        assert_eq!(after, before, "a re-built paragraph is bit-for-bit the original");
        assert!(
            t.shape_count + t.rewrap_count > shapes + rewraps,
            "evicted entries rebuild on demand (full shape or template rewrap)"
        );
    }

    #[test]
    fn prewarm_is_opt_in_dedup_and_identity_preserving() {
        let c = content("prewarm this paragraph and then measure it", 16.0);
        // Baseline: shape on demand in a fresh system.
        let mut baseline = TextSystem::new();
        let expected = snapshot(&baseline.paragraph(&c, Some(240.0)));
        let expected_shapes = baseline.shape_count;

        // Prewarm a second system, then look the paragraph up again.
        let mut warmed = TextSystem::new();
        warmed.prewarm(std::iter::once((&c, Some(240.0))));
        assert_eq!(warmed.shape_count, expected_shapes, "prewarm shapes each item exactly once");
        let shapes_after_prewarm = warmed.shape_count;
        assert_eq!(snapshot(&warmed.paragraph(&c, Some(240.0))), expected, "prewarm result identical");
        assert_eq!(warmed.shape_count, shapes_after_prewarm, "post-prewarm lookup is a cache hit");
        assert_eq!(snapshot(&warmed.paragraph(&c, Some(240.0))), expected);
    }

    #[test]
    fn instrumentation_counters_are_monotonic() {
        let mut t = TextSystem::new();
        let (shapes, nanos, hits) = (t.shape_count, t.shape_nanos, t.cache_hits);
        let c = content("instrumentation counters must advance with real work", 16.0);
        t.measure(&c, Some(200.0));
        assert!(t.shape_count > shapes, "shape_count advances");
        assert!(t.shape_nanos >= nanos, "shape_nanos never decreases");
        assert!(t.cache_hits >= hits);
        // The font-DB phase timer reports the background load after the first font use.
        assert!(super::last_font_db_load_nanos() > 0, "the loader thread has finished by now");
        let _ = super::last_font_db_cache_hit();
    }

    /// Isolate the font-DB cache in a fresh temp dir; restores the env after.
    /// A process-wide lock serializes the cache tests against each other because
    /// `RENDRIX_CACHE_DIR` is process-global (other tests using `TextSystem::new`
    /// concurrently are unaffected: they just full-scan or hit a valid cache).
    static FONT_CACHE_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    fn with_font_cache_dir(f: impl FnOnce()) {
        let _guard = FONT_CACHE_LOCK.lock().unwrap();
        use std::sync::atomic::{AtomicU64, Ordering};
        static N: AtomicU64 = AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!("rendrix-fontcache-test-{}-{}", std::process::id(), N.fetch_add(1, Ordering::SeqCst)));
        let _ = std::fs::remove_dir_all(&dir);
        let prev_dir = std::env::var_os("RENDRIX_CACHE_DIR");
        let prev_off = std::env::var_os("RENDRIX_NO_FONT_CACHE");
        std::env::set_var("RENDRIX_CACHE_DIR", &dir);
        std::env::remove_var("RENDRIX_NO_FONT_CACHE");
        f();
        if let Some(v) = prev_dir {
            std::env::set_var("RENDRIX_CACHE_DIR", v);
        } else {
            std::env::remove_var("RENDRIX_CACHE_DIR");
        }
        if let Some(v) = prev_off {
            std::env::set_var("RENDRIX_NO_FONT_CACHE", v);
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn font_db_cache_roundtrip_preserves_faces() {
        with_font_cache_dir(|| {
            let locale = sys_locale::get_locale().unwrap_or_else(|| "en-US".to_string());
            // Cold: no cache file yet.
            assert!(load_font_db_cache(&locale).is_none(), "miss on empty dir");
            // A full scan populates the cache as a side effect. The DB loads on a
            // background thread now, so join it before asserting on the file.
            let mut sys = TextSystem::new();
            let _ = sys.fonts();
            assert!(font_cache_path().is_file(), "scan writes the cache");
            let db = load_font_db_cache(&locale).expect("roundtrip hit");
            let (mut a, mut b): (Vec<_>, Vec<_>) =
                (sys.fonts().db().faces().collect(), db.faces().collect());
            assert_eq!(a.len(), b.len(), "no faces lost");
            assert!(!a.is_empty(), "system has fonts");
            // Both iterate in insertion order; spot-check the first 32 faces field by field.
            a.truncate(32);
            b.truncate(32);
            for (x, y) in a.iter().zip(b.iter()) {
                assert_eq!(x.index, y.index);
                assert_eq!(x.families, y.families);
                assert_eq!(x.post_script_name, y.post_script_name);
                assert_eq!(x.style, y.style);
                assert_eq!(x.weight, y.weight);
                assert_eq!(x.stretch, y.stretch);
                assert_eq!(x.monospaced, y.monospaced);
                match (&x.source, &y.source) {
                    (fontdb::Source::File(p), fontdb::Source::File(q)) => assert_eq!(p, q),
                    other => panic!("system faces are file-backed: {other:?}"),
                }
            }
            // A cache-built system resolves the same families as a scanned one.
            let mut cached = TextSystem::new();
            let _ = cached.fonts();
            assert_eq!(cached.installed, sys.installed, "same installed-family set");
        });
    }

    #[test]
    fn font_db_cache_rejects_garbage_and_tampering() {
        with_font_cache_dir(|| {
            let locale = sys_locale::get_locale().unwrap_or_else(|| "en-US".to_string());
            let path = font_cache_path();
            // Garbage bytes.
            if let Some(parent) = path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            std::fs::write(&path, b"definitely not a font cache").unwrap();
            assert!(load_font_db_cache(&locale).is_none(), "garbage rejected");
            // Truncated magic.
            std::fs::write(&path, &FONT_CACHE_MAGIC.to_le_bytes()[..2]).unwrap();
            assert!(load_font_db_cache(&locale).is_none(), "truncation rejected");
            // Valid cache, then tampered: wrong locale, trailing bytes, truncation.
            // The scan (and cache write) happen on the loader thread; join it first.
            let mut _sys = TextSystem::new();
            let _ = _sys.fonts();
            assert!(load_font_db_cache(&locale).is_some());
            assert!(load_font_db_cache("xx-OTHER-LOCALE-xx").is_none(), "locale-keyed");
            let mut bytes = std::fs::read(&path).unwrap();
            bytes.push(0xAA);
            std::fs::write(&path, &bytes).unwrap();
            assert!(load_font_db_cache(&locale).is_none(), "trailing bytes rejected");
            bytes.pop();
            bytes.pop();
            std::fs::write(&path, &bytes).unwrap();
            assert!(load_font_db_cache(&locale).is_none(), "truncation rejected");
            // Oversized family count guard.
            assert!(language_from_disc(205).is_none(), "out-of-range language rejected");
            assert!(language_from_disc(u32::MAX).is_none());
            assert!(width_from_number(0).is_none() && width_from_number(10).is_none());
        });
    }
}
