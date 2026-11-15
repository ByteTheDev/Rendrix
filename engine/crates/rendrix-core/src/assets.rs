//! Asset access and decoded image storage.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

/// Where the engine reads relative resources (stylesheets, images, fonts).
pub trait AssetProvider {
    fn read(&self, path: &str) -> Option<Vec<u8>>;
}

fn normalize(path: &str) -> String {
    let p = path.split(['?', '#']).next().unwrap_or(path);
    let p = p.trim_start_matches("./").trim_start_matches('/');
    p.replace('\\', "/")
}

#[derive(Default)]
pub struct MapAssets(pub HashMap<String, Vec<u8>>);

impl AssetProvider for MapAssets {
    fn read(&self, path: &str) -> Option<Vec<u8>> {
        self.0.get(&normalize(path)).cloned()
    }
}

/// Reads from a directory, refusing paths that escape it.
pub struct DirAssets(pub PathBuf);

impl AssetProvider for DirAssets {
    fn read(&self, path: &str) -> Option<Vec<u8>> {
        let rel = normalize(path);
        if rel.split('/').any(|c| c == "..") {
            return None;
        }
        std::fs::read(self.0.join(rel)).ok()
    }
}

#[derive(Clone)]
pub struct ImageEntry {
    pub id: u32,
    pub width: u32,
    pub height: u32,
    pub rgba: Arc<Vec<u8>>,
    /// Mean alpha (0..1) of the decoded bitmap, computed once at decode time.
    /// Used by `mask-image` (average-alpha masking) without re-scanning pixels per frame.
    pub avg_alpha: f32,
}

fn average_alpha(rgba: &[u8]) -> f32 {
    if rgba.is_empty() {
        return 0.0;
    }
    let sum: u64 = rgba.chunks_exact(4).map(|p| p[3] as u64).sum();
    sum as f32 / (rgba.len() / 4) as f32 / 255.0
}

#[derive(Default)]
pub struct Images {
    by_url: HashMap<String, Option<ImageEntry>>,
    next_id: u32,
    pub pending_upload: Vec<ImageEntry>,
    /// Rasterized inline `<svg>` subtrees, keyed by hosting element id. The entry is
    /// keyed on the serialized markup plus the raster size, so DOM or layout changes
    /// re-rasterize while identical frames reuse the uploaded texture.
    svg_inline: HashMap<u32, (String, u32, u32, ImageEntry)>,
}

impl Images {
    /// Decode an image the first time it is requested. Failed loads are remembered.
    pub fn get(&mut self, url: &str, assets: &dyn AssetProvider) -> Option<ImageEntry> {
        if let Some(e) = self.by_url.get(url) {
            return e.clone();
        }
        let entry = assets.read(url).and_then(|bytes| decode(&bytes)).map(|(w, h, rgba)| {
            let avg_alpha = average_alpha(&rgba);
            self.next_id += 1;
            ImageEntry { id: self.next_id, width: w, height: h, rgba: Arc::new(rgba), avg_alpha }
        });
        if let Some(e) = &entry {
            self.pending_upload.push(e.clone());
        }
        self.by_url.insert(url.to_string(), entry.clone());
        entry
    }

    /// Rasterize an inline `<svg>` subtree at `w`×`h` px (cached by markup + size).
    /// Returns `None` for empty sizes or unparseable markup; failures are not cached.
    pub fn svg(&mut self, node: u32, html: &str, w: u32, h: u32) -> Option<ImageEntry> {
        if w == 0 || h == 0 {
            return None;
        }
        if let Some((prev_html, prev_w, prev_h, entry)) = self.svg_inline.get(&node) {
            if prev_html == html && *prev_w == w && *prev_h == h {
                return Some(entry.clone());
            }
        }
        let (rw, rh, rgba) = rendrix_gfx::svg::rasterize(html.as_bytes(), w.min(2048), h.min(2048))?;
        self.next_id += 1;
        let avg_alpha = average_alpha(&rgba);
        let entry = ImageEntry { id: self.next_id, width: rw, height: rh, rgba: Arc::new(rgba), avg_alpha };
        self.pending_upload.push(entry.clone());
        self.svg_inline.insert(node, (html.to_string(), w, h, entry.clone()));
        Some(entry)
    }

    /// Number of decoded images waiting for GPU upload. Used by tests and the
    /// future per-frame upload budget; the drivers currently drain all.
    #[allow(dead_code)]
    pub fn pending_count(&self) -> usize {
        self.pending_upload.len()
    }

    /// Drain up to `n` pending uploads, oldest first, so the scheduler can
    /// bound per-frame upload work without taking the whole queue.
    /// Not wired yet: capping needs a pending-upload repaint wake (the renderer
    /// skips not-yet-uploaded images), so the drivers still drain all.
    #[allow(dead_code)]
    pub fn take_up_to(&mut self, n: usize) -> Vec<ImageEntry> {
        let k = self.pending_upload.len().min(n);
        self.pending_upload.drain(..k).collect()
    }
}

pub fn decode(bytes: &[u8]) -> Option<(u32, u32, Vec<u8>)> {
    // SVG first: the `image` crate cannot decode SVG, and probing by content keeps
    // `.svg` files working regardless of extension (data URLs, extensionless routes).
    if rendrix_gfx::svg::is_svg_bytes(bytes) {
        if let Some(raster) = rendrix_gfx::svg::rasterize_at_intrinsic(bytes) {
            return Some(raster);
        }
        // An unparseable SVG falls through to the image decoder (which will fail),
        // so failed loads are still remembered as failures.
    }
    let img = image::load_from_memory(bytes).ok()?.to_rgba8();
    let (w, h) = img.dimensions();
    Some((w, h, img.into_raw()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn png_bytes() -> Vec<u8> {
        let img = image::RgbaImage::from_pixel(2, 3, image::Rgba([10, 20, 30, 255]));
        let mut png = Vec::new();
        image::DynamicImage::ImageRgba8(img).write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png).unwrap();
        png
    }

    #[test]
    fn uploads_drain_bounded_oldest_first() {
        let mut assets = MapAssets::default();
        assets.0.insert("a.png".into(), png_bytes());
        assets.0.insert("b.png".into(), png_bytes());
        let mut imgs = Images::default();
        let a = imgs.get("a.png", &assets).expect("decodes");
        let b = imgs.get("b.png", &assets).expect("decodes");
        assert_ne!(a.id, b.id);
        assert_eq!((a.width, a.height), (2, 3));
        assert_eq!(imgs.pending_count(), 2);
        // Repeat requests share the decode and queue nothing new.
        assert!(imgs.get("a.png", &assets).is_some());
        assert_eq!(imgs.pending_count(), 2);
        // Failed loads are remembered without queueing.
        assert!(imgs.get("missing.png", &assets).is_none());
        assert_eq!(imgs.pending_count(), 2);
        let one = imgs.take_up_to(1);
        assert_eq!(one.len(), 1);
        assert_eq!(one[0].id, a.id, "oldest first");
        assert_eq!(imgs.pending_count(), 1);
        let rest = imgs.take_up_to(99);
        assert_eq!(rest.len(), 1);
        assert_eq!(rest[0].id, b.id);
        assert_eq!(imgs.pending_count(), 0);
    }
}

