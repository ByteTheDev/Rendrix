//! Per-`<canvas>` 2D raster state (A3, approved subset).
//!
//! Each `<canvas>` owns a straight-alpha RGBA buffer rasterized here on the CPU. The GPU side
//! (`Renderer::upload_canvas` / `update_canvas_region`) turns the buffer into a texture and
//! `rendrix-core/src/paint.rs` composites it as one `Item::Image`, so clips, transforms and
//! opacity apply exactly like any other image. When no `<canvas>` exists this module is never
//! touched: style/layout/paint/GPU paths are bit-identical (see the invalidation test).
//!
//! ## Approved subset
//! State: `save`/`restore` (style + transform only, **not** the path — per spec), `fillStyle` /
//! `strokeStyle` (CSS color string or 2-stop+ linear gradient), `lineWidth`, `lineCap`
//! (`butt`/`round`/`square`), `lineJoin` (`miter` with limit 10 falling back to `bevel`, `round`,
//! `bevel`), `globalAlpha`.
//! Paths: `beginPath`/`moveTo`/`lineTo`/`rect`/`arc`/`ellipse`/`closePath`, `fill` (nonzero
//! winding) / `stroke` (centered, width scaled by `sqrt(|det CTM|)`), `fillRect`/`strokeRect` /
//! `clearRect` (all CTM-transformed; `putImageData` is not).
//! Paint: `createLinearGradient` (offsets are clamped to `[0,1]` and sorted; one stop behaves as
//! a solid, two or more interpolate — there is no stop-count clamp on this path),
//! `drawImage` (image or canvas source, nearest-neighbour, `globalAlpha` applies, `smoothing`
//! flags ignored), `getImageData`/`putImageData` (raw round-trip; large reads base64-encode the
//! whole rect — **perf caveat**: prefer small rects), `canvas.width/height` (setting either
//! clears the bitmap **and** resets context state, per spec), `toDataURL` (`image/png` only).
//! Miter joins use limit 10, acute spikes fall back to bevel. Anti-aliasing: none (hard 50%
//! coverage test); text (`fillText`/`strokeText`), shadows, patterns, radial gradients,
//! `globalCompositeOperation`, filters and hit regions are **not** implemented (see report).

use rendrix_css::Color;
use std::collections::{HashMap, HashSet};
use std::sync::{Mutex, OnceLock};

/// Maximum canvas edge in pixels. Larger attribute values are clamped; the clamp is observable
/// through `width`/`height` (attributes keep their value, the bitmap clamps).
pub const MAX_CANVAS_DIM: u32 = 4096;
/// Miter limit, matching the Canvas2D default.
const MITER_LIMIT: f32 = 10.0;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum LineCap {
    Butt,
    Round,
    Square,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum LineJoin {
    Miter,
    Round,
    Bevel,
}

#[derive(Clone, Debug, PartialEq)]
pub struct LinearGradient {
    pub x0: f32,
    pub y0: f32,
    pub x1: f32,
    pub y1: f32,
    /// `(offset, color)`, offsets clamped to `[0,1]` and sorted ascending.
    pub stops: Vec<(f32, Color)>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Paint {
    Solid(Color),
    Linear(LinearGradient),
}

impl Paint {
    fn alpha(&self) -> f32 {
        match self {
            Paint::Solid(c) => c.a,
            Paint::Linear(g) => g.stops.iter().map(|(_, c)| c.a).fold(1.0f32, f32::min),
        }
    }
}

#[derive(Clone, Debug)]
enum PathCmd {
    Move([f32; 2]),
    Line([f32; 2]),
    Rect([f32; 4]),
    Circle {
        c: [f32; 2],
        r: f32,
        a0: f32,
        sweep: f32,
    },
    Ellipse {
        c: [f32; 2],
        r: [f32; 2],
        rot: f32,
        a0: f32,
        sweep: f32,
    },
    Close,
}

#[derive(Clone, Debug)]
struct Subpath {
    pts: Vec<[f32; 2]>,
    closed: bool,
}

#[derive(Clone, Debug)]
struct Saved {
    xf: [f32; 6],
    fill: Paint,
    stroke: Paint,
    line_width: f32,
    cap: LineCap,
    join: LineJoin,
    alpha: f32,
}

/// CPU-side 2D context of one `<canvas>`. All coordinates in user space until `fill`/`stroke`
/// time, when the CTM is applied. Straight-alpha RGBA, row-major.
#[derive(Clone, Debug)]
pub struct Canvas2D {
    width: u32,
    height: u32,
    pixels: Vec<u8>,
    xf: [f32; 6],
    stack: Vec<Saved>,
    fill: Paint,
    stroke: Paint,
    line_width: f32,
    cap: LineCap,
    join: LineJoin,
    alpha: f32,
    path: Vec<PathCmd>,
    dirty: Option<[u32; 4]>,
}

impl Canvas2D {
    pub fn new(width: u32, height: u32) -> Canvas2D {
        let (w, h) = (width.min(MAX_CANVAS_DIM), height.min(MAX_CANVAS_DIM));
        Canvas2D {
            width: w,
            height: h,
            pixels: vec![0u8; (w * h * 4) as usize],
            xf: [1.0, 0.0, 0.0, 1.0, 0.0, 0.0],
            stack: Vec::new(),
            fill: Paint::Solid(Color::rgba(0.0, 0.0, 0.0, 1.0)),
            stroke: Paint::Solid(Color::rgba(0.0, 0.0, 0.0, 1.0)),
            line_width: 1.0,
            cap: LineCap::Butt,
            join: LineJoin::Miter,
            alpha: 1.0,
            path: Vec::new(),
            dirty: None,
        }
    }

    pub fn size(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    pub fn pixels(&self) -> &[u8] {
        &self.pixels
    }

    /// Resize clears the bitmap and resets all context state (matches the spec for setting
    /// `canvas.width`/`height`).
    pub fn resize(&mut self, width: u32, height: u32) {
        *self = Canvas2D::new(width, height);
    }

    fn mark(&mut self, x: u32, y: u32, w: u32, h: u32) {
        if w == 0 || h == 0 {
            return;
        }
        let r = [x, y, w, h];
        self.dirty = Some(match self.dirty {
            None => r,
            Some(d) => {
                let x0 = d[0].min(x);
                let y0 = d[1].min(y);
                let x1 = (d[0] + d[2]).max(x + w);
                let y1 = (d[1] + d[3]).max(y + h);
                [x0, y0, x1 - x0, y1 - y0]
            }
        });
    }

    pub fn take_dirty(&mut self) -> Option<[u32; 4]> {
        self.dirty.take()
    }

    /// Mark the whole bitmap dirty (used when an attribute resize changes the size outside any
    /// draw op, so the next upload refreshes the recreated GPU target).
    pub fn mark_full(&mut self) {
        if self.width > 0 && self.height > 0 {
            self.dirty = Some([0, 0, self.width, self.height]);
        }
    }

    // ---- state ---------------------------------------------------------------------------

    pub fn save(&mut self) {
        self.stack.push(Saved {
            xf: self.xf,
            fill: self.fill.clone(),
            stroke: self.stroke.clone(),
            line_width: self.line_width,
            cap: self.cap,
            join: self.join,
            alpha: self.alpha,
        });
    }

    pub fn restore(&mut self) {
        if let Some(s) = self.stack.pop() {
            self.xf = s.xf;
            self.fill = s.fill;
            self.stroke = s.stroke;
            self.line_width = s.line_width;
            self.cap = s.cap;
            self.join = s.join;
            self.alpha = s.alpha;
        }
    }

    /// `setTransform`: non-finite values are ignored (per spec).
    pub fn set_transform(&mut self, m: [f32; 6]) {
        if m.iter().all(|v| v.is_finite()) {
            self.xf = m;
        }
    }

    /// `translate`/`scale`/`rotate` post-multiply: non-finite values are ignored (per spec).
    pub fn transform(&mut self, m: [f32; 6]) {
        if m.iter().all(|v| v.is_finite()) {
            self.xf = mul_affine(self.xf, m);
        }
    }

    pub fn set_fill(&mut self, p: Paint) {
        self.fill = p;
    }

    pub fn set_stroke(&mut self, p: Paint) {
        self.stroke = p;
    }

    /// Non-finite or non-positive widths are ignored (per spec).
    pub fn set_line_width(&mut self, w: f32) {
        if w.is_finite() && w > 0.0 {
            self.line_width = w;
        }
    }

    pub fn set_line_cap(&mut self, c: LineCap) {
        self.cap = c;
    }

    pub fn set_line_join(&mut self, j: LineJoin) {
        self.join = j;
    }

    /// Values outside `[0,1]` (or non-finite) are ignored (per spec).
    pub fn set_alpha(&mut self, a: f32) {
        if a.is_finite() && (0.0..=1.0).contains(&a) {
            self.alpha = a;
        }
    }

    // ---- paths ---------------------------------------------------------------------------

    pub fn begin_path(&mut self) {
        self.path.clear();
    }

    pub fn move_to(&mut self, x: f32, y: f32) {
        self.path.push(PathCmd::Move([x, y]));
    }

    pub fn line_to(&mut self, x: f32, y: f32) {
        self.path.push(PathCmd::Line([x, y]));
    }

    pub fn rect(&mut self, x: f32, y: f32, w: f32, h: f32) {
        self.path.push(PathCmd::Rect([x, y, w, h]));
    }

    /// Negative radius is an error (the JS layer maps it to `IndexSizeError`).
    pub fn arc(
        &mut self,
        x: f32,
        y: f32,
        r: f32,
        a0: f32,
        a1: f32,
        ccw: bool,
    ) -> Result<(), String> {
        if !r.is_finite() || r < 0.0 {
            return Err("IndexSizeError: arc radius must be non-negative".to_string());
        }
        if r == 0.0 {
            return Ok(());
        }
        self.path.push(PathCmd::Circle {
            c: [x, y],
            r,
            a0,
            sweep: sweep_of(a0, a1, ccw),
        });
        Ok(())
    }

    /// Spec-shaped parameter list (center, radii, rotation, angles, direction).
    #[allow(clippy::too_many_arguments)]
    pub fn ellipse(
        &mut self,
        x: f32,
        y: f32,
        rx: f32,
        ry: f32,
        rot: f32,
        a0: f32,
        a1: f32,
        ccw: bool,
    ) -> Result<(), String> {
        if !rx.is_finite() || !ry.is_finite() || rx < 0.0 || ry < 0.0 {
            return Err("IndexSizeError: ellipse radii must be non-negative".to_string());
        }
        if rx == 0.0 || ry == 0.0 {
            return Ok(());
        }
        self.path.push(PathCmd::Ellipse {
            c: [x, y],
            r: [rx, ry],
            rot,
            a0,
            sweep: sweep_of(a0, a1, ccw),
        });
        Ok(())
    }

    pub fn close_path(&mut self) {
        self.path.push(PathCmd::Close);
    }

    // ---- paint ---------------------------------------------------------------------------

    pub fn fill(&mut self) {
        if self.width == 0 || self.height == 0 {
            return;
        }
        let subs = flatten(&self.path);
        let inv = invert_affine(self.xf);
        let paint = self.fill.clone();
        let alpha = self.alpha;
        self.fill_subpaths(&subs, &paint, alpha, inv, false);
    }

    pub fn stroke(&mut self) {
        if self.width == 0 || self.height == 0 {
            return;
        }
        let subs = flatten(&self.path);
        let inv = invert_affine(self.xf);
        let paint = self.stroke.clone();
        let (w, cap, join, alpha) = (self.line_width, self.cap, self.join, self.alpha);
        self.stroke_subpaths(&subs, &paint, w, cap, join, alpha, inv);
    }

    pub fn fill_rect(&mut self, x: f32, y: f32, w: f32, h: f32) {
        if self.width == 0 || self.height == 0 {
            return;
        }
        let subs = vec![Subpath {
            pts: vec![[x, y], [x + w, y], [x + w, y + h], [x, y + h]],
            closed: true,
        }];
        let inv = invert_affine(self.xf);
        let paint = self.fill.clone();
        let alpha = self.alpha;
        self.fill_subpaths(&subs, &paint, alpha, inv, false);
    }

    pub fn stroke_rect(&mut self, x: f32, y: f32, w: f32, h: f32) {
        if self.width == 0 || self.height == 0 {
            return;
        }
        let subs = vec![Subpath {
            pts: vec![[x, y], [x + w, y], [x + w, y + h], [x, y + h]],
            closed: true,
        }];
        let inv = invert_affine(self.xf);
        let paint = self.stroke.clone();
        let (w, cap, join, alpha) = (self.line_width, self.cap, self.join, self.alpha);
        self.stroke_subpaths(&subs, &paint, w, cap, join, alpha, inv);
    }

    /// `clearRect` is CTM-transformed (unlike `putImageData`); cleared pixels become transparent.
    pub fn clear_rect(&mut self, x: f32, y: f32, w: f32, h: f32) {
        if self.width == 0 || self.height == 0 {
            return;
        }
        let subs = vec![Subpath {
            pts: vec![[x, y], [x + w, y], [x + w, y + h], [x, y + h]],
            closed: true,
        }];
        let inv = invert_affine(self.xf);
        self.fill_subpaths(&subs, &Paint::Solid(Color::TRANSPARENT), 1.0, inv, true);
    }

    /// `drawImage` subset: blit `src` (straight-alpha, `sw`×`sh`) from the `sw`×`sh` rect at
    /// `(sx,sy)` into the user-space rect `(dx,dy,dw,dh)`, inverse-mapped through the CTM so
    /// translate/scale/rotate all apply. Nearest-neighbour; `globalAlpha` applies.
    /// Spec-shaped parameter list (source URL/bitmap + source rect + dest rect).
    #[allow(clippy::too_many_arguments)]
    pub fn draw_image(
        &mut self,
        src: &[u8],
        sw: u32,
        sh: u32,
        sx: f32,
        sy: f32,
        sw_: f32,
        sh_: f32,
        dx: f32,
        dy: f32,
        dw: f32,
        dh: f32,
    ) -> Result<(), String> {
        if sw_ == 0.0 || sh_ == 0.0 || dw == 0.0 || dh == 0.0 {
            return Ok(());
        }
        if self.width == 0 || self.height == 0 {
            return Ok(());
        }
        let Some(inv) = invert_affine(self.xf) else {
            return Ok(());
        };
        let corners = [[dx, dy], [dx + dw, dy], [dx + dw, dy + dh], [dx, dy + dh]]
            .map(|p| apply_affine(self.xf, p));
        let (mut x0, mut y0, mut x1, mut y1) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
        for p in corners {
            x0 = x0.min(p[0]);
            y0 = y0.min(p[1]);
            x1 = x1.max(p[0]);
            y1 = y1.max(p[1]);
        }
        let (ix0, iy0, ix1, iy1) = (
            (x0.floor() as i32).max(0).min(self.width as i32),
            (y0.floor() as i32).max(0).min(self.height as i32),
            (x1.ceil() as i32).max(0).min(self.width as i32),
            (y1.ceil() as i32).max(0).min(self.height as i32),
        );
        if ix0 >= ix1 || iy0 >= iy1 {
            return Ok(());
        }
        let alpha = self.alpha;
        for y in iy0..iy1 {
            for x in ix0..ix1 {
                let u = apply_affine(inv, [x as f32 + 0.5, y as f32 + 0.5]);
                let fx = sx + (u[0] - dx) / dw * sw_;
                let fy = sy + (u[1] - dy) / dh * sh_;
                if fx < 0.0 || fy < 0.0 || fx >= sw as f32 || fy >= sh as f32 {
                    continue;
                }
                let si = ((fy as u32) * sw + (fx as u32)) as usize * 4;
                if si + 3 >= src.len() {
                    continue;
                }
                let (sr, sg, sb, sa) = (
                    src[si] as f32 / 255.0,
                    src[si + 1] as f32 / 255.0,
                    src[si + 2] as f32 / 255.0,
                    src[si + 3] as f32 / 255.0 * alpha,
                );
                if sa <= 0.0 {
                    continue;
                }
                let di = (y as u32 * self.width + x as u32) as usize * 4;
                blend(&mut self.pixels[di..di + 4], sr, sg, sb, sa);
            }
        }
        self.mark(
            ix0 as u32,
            iy0 as u32,
            (ix1 - ix0) as u32,
            (iy1 - iy0) as u32,
        );
        Ok(())
    }

    /// Read a straight-alpha rect; out-of-canvas pixels are transparent (per spec).
    pub fn get_data(&self, x: i32, y: i32, w: u32, h: u32) -> Vec<u8> {
        let mut out = vec![0u8; (w * h * 4) as usize];
        for row in 0..h {
            for col in 0..w {
                let (sx, sy) = (x + col as i32, y + row as i32);
                if sx < 0 || sy < 0 || sx >= self.width as i32 || sy >= self.height as i32 {
                    continue;
                }
                let si = (sy as u32 * self.width + sx as u32) as usize * 4;
                let di = (row * w + col) as usize * 4;
                out[di..di + 4].copy_from_slice(&self.pixels[si..si + 4]);
            }
        }
        out
    }

    /// Raw copy (ignores CTM and `globalAlpha`, per spec).
    pub fn put_data(&mut self, data: &[u8], w: u32, h: u32, dx: i32, dy: i32) {
        for row in 0..h {
            for col in 0..w {
                let (sx, sy) = (dx + col as i32, dy + row as i32);
                if sx < 0 || sy < 0 || sx >= self.width as i32 || sy >= self.height as i32 {
                    continue;
                }
                let si = (row * w + col) as usize * 4;
                let di = (sy as u32 * self.width + sx as u32) as usize * 4;
                if si + 3 < data.len() {
                    self.pixels[di..di + 4].copy_from_slice(&data[si..si + 4]);
                }
            }
        }
        self.mark(
            dx.max(0) as u32,
            dy.max(0) as u32,
            w.min(self.width),
            h.min(self.height),
        );
    }

    /// PNG bytes for `toDataURL` (`None` when the bitmap is empty).
    pub fn to_png_bytes(&self) -> Option<Vec<u8>> {
        use image::ImageEncoder;
        if self.width == 0 || self.height == 0 {
            return None;
        }
        let mut buf = Vec::new();
        let enc = image::codecs::png::PngEncoder::new(&mut buf);
        enc.write_image(
            &self.pixels,
            self.width,
            self.height,
            image::ExtendedColorType::Rgba8,
        )
        .ok()?;
        Some(buf)
    }

    // ---- internals -------------------------------------------------------------------------

    fn sample(&self, paint: &Paint, inv: Option<[f32; 6]>, dx: f32, dy: f32) -> [f32; 4] {
        match paint {
            Paint::Solid(c) => [c.r, c.g, c.b, c.a],
            Paint::Linear(g) => {
                // Sample in user space so the gradient follows the CTM like the geometry does.
                let (ux, uy) = match inv {
                    Some(m) => {
                        let p = apply_affine(m, [dx, dy]);
                        (p[0], p[1])
                    }
                    None => (dx, dy),
                };
                let (vx, vy) = (g.x1 - g.x0, g.y1 - g.y0);
                let len2 = (vx * vx + vy * vy).max(1e-12);
                let t = (((ux - g.x0) * vx + (uy - g.y0) * vy) / len2).clamp(0.0, 1.0);
                gradient_at(&g.stops, t)
            }
        }
    }

    fn fill_subpaths(
        &mut self,
        subs: &[Subpath],
        paint: &Paint,
        alpha: f32,
        inv: Option<[f32; 6]>,
        clear: bool,
    ) {
        if inv.is_none() {
            return;
        }
        if !clear && paint.alpha() * alpha <= 0.0 {
            return;
        }
        // Device-space polygons.
        let polys: Vec<(Vec<[f32; 2]>, bool)> = subs
            .iter()
            .map(|s| {
                (
                    s.pts
                        .iter()
                        .map(|p| apply_affine(self.xf, *p))
                        .collect::<Vec<[f32; 2]>>(),
                    s.closed,
                )
            })
            .filter(|(p, _)| p.len() >= 2)
            .collect();
        let (mut x0, mut y0, mut x1, mut y1) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
        for (p, _) in &polys {
            for q in p {
                x0 = x0.min(q[0]);
                y0 = y0.min(q[1]);
                x1 = x1.max(q[0]);
                y1 = y1.max(q[1]);
            }
        }
        if x0 > x1 || y0 > y1 {
            return;
        }
        let (ix0, iy0, ix1, iy1) = (
            (x0.floor() as i32).max(0).min(self.width as i32),
            (y0.floor() as i32).max(0).min(self.height as i32),
            (x1.ceil() as i32).max(0).min(self.width as i32),
            (y1.ceil() as i32).max(0).min(self.height as i32),
        );
        if ix0 >= ix1 || iy0 >= iy1 {
            return;
        }
        // Open subpaths are implicitly closed for fill (per spec).
        for y in iy0..iy1 {
            for x in ix0..ix1 {
                let p = [x as f32 + 0.5, y as f32 + 0.5];
                if !inside_nonzero(&polys, p) {
                    continue;
                }
                let di = (y as u32 * self.width + x as u32) as usize * 4;
                if clear {
                    self.pixels[di..di + 4].copy_from_slice(&[0, 0, 0, 0]);
                } else {
                    let c = self.sample(paint, inv, x as f32 + 0.5, y as f32 + 0.5);
                    blend(&mut self.pixels[di..di + 4], c[0], c[1], c[2], c[3] * alpha);
                }
            }
        }
        self.mark(
            ix0 as u32,
            iy0 as u32,
            (ix1 - ix0) as u32,
            (iy1 - iy0) as u32,
        );
    }

    #[allow(clippy::too_many_arguments)]
    fn stroke_subpaths(
        &mut self,
        subs: &[Subpath],
        paint: &Paint,
        width: f32,
        cap: LineCap,
        join: LineJoin,
        alpha: f32,
        inv: Option<[f32; 6]>,
    ) {
        if inv.is_none() || paint.alpha() * alpha <= 0.0 {
            return;
        }
        let det = (self.xf[0] * self.xf[3] - self.xf[1] * self.xf[2]).abs();
        let w = (width * det.sqrt()).max(0.5);
        let half = w / 2.0;
        let polys: Vec<(Vec<[f32; 2]>, bool)> = subs
            .iter()
            .map(|s| {
                (
                    s.pts.iter().map(|p| apply_affine(self.xf, *p)).collect(),
                    s.closed,
                )
            })
            .collect();
        let (mut x0, mut y0, mut x1, mut y1) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
        for (p, _) in &polys {
            for q in p {
                x0 = x0.min(q[0] - half - 1.0);
                y0 = y0.min(q[1] - half - 1.0);
                x1 = x1.max(q[0] + half + 1.0);
                y1 = y1.max(q[1] + half + 1.0);
            }
        }
        if x0 > x1 || y0 > y1 {
            return;
        }
        let (ix0, iy0, ix1, iy1) = (
            (x0.floor() as i32).max(0).min(self.width as i32),
            (y0.floor() as i32).max(0).min(self.height as i32),
            (x1.ceil() as i32).max(0).min(self.width as i32),
            (y1.ceil() as i32).max(0).min(self.height as i32),
        );
        if ix0 >= ix1 || iy0 >= iy1 {
            return;
        }
        for y in iy0..iy1 {
            for x in ix0..ix1 {
                let p = [x as f32 + 0.5, y as f32 + 0.5];
                if !stroke_hit(&polys, p, half, cap, join) {
                    continue;
                }
                let di = (y as u32 * self.width + x as u32) as usize * 4;
                let c = self.sample(paint, inv, x as f32 + 0.5, y as f32 + 0.5);
                blend(&mut self.pixels[di..di + 4], c[0], c[1], c[2], c[3] * alpha);
            }
        }
        self.mark(
            ix0 as u32,
            iy0 as u32,
            (ix1 - ix0) as u32,
            (iy1 - iy0) as u32,
        );
    }
}

// ---- path flattening ------------------------------------------------------------------------

fn sweep_of(a0: f32, a1: f32, ccw: bool) -> f32 {
    const TAU: f32 = std::f32::consts::TAU;
    if ccw {
        let mut s = -(a0 - a1).rem_euclid(TAU);
        if s == 0.0 && a0 != a1 {
            s = -TAU;
        }
        s
    } else {
        let mut s = (a1 - a0).rem_euclid(TAU);
        if s == 0.0 && a0 != a1 {
            s = TAU;
        }
        s
    }
}

fn arc_points(c: [f32; 2], r: [f32; 2], rot: f32, a0: f32, sweep: f32) -> Vec<[f32; 2]> {
    let approx = (sweep.abs() * r[0].max(r[1]) * 0.5)
        .ceil()
        .clamp(8.0, 512.0) as usize;
    let (cr, sr) = (rot.cos(), rot.sin());
    (0..=approx)
        .map(|i| {
            let a = a0 + sweep * i as f32 / approx as f32;
            let (x, y) = (r[0] * a.cos(), r[1] * a.sin());
            [c[0] + x * cr - y * sr, c[1] + x * sr + y * cr]
        })
        .collect()
}

fn flatten(cmds: &[PathCmd]) -> Vec<Subpath> {
    let mut out: Vec<Subpath> = Vec::new();
    let mut cur: Option<Subpath> = None;
    let flush = |cur: &mut Option<Subpath>, out: &mut Vec<Subpath>| {
        if let Some(s) = cur.take() {
            if !s.pts.is_empty() {
                out.push(s);
            }
        }
    };
    for cmd in cmds {
        match cmd {
            PathCmd::Move(p) => {
                flush(&mut cur, &mut out);
                cur = Some(Subpath {
                    pts: vec![*p],
                    closed: false,
                });
            }
            PathCmd::Line(p) => match &mut cur {
                Some(s) => s.pts.push(*p),
                None => {
                    cur = Some(Subpath {
                        pts: vec![*p],
                        closed: false,
                    })
                }
            },
            PathCmd::Rect([x, y, w, h]) => {
                flush(&mut cur, &mut out);
                out.push(Subpath {
                    pts: vec![[*x, *y], [*x + *w, *y], [*x + *w, *y + *h], [*x, *y + *h]],
                    closed: true,
                });
                cur = Some(Subpath {
                    pts: vec![[*x, *y]],
                    closed: false,
                });
            }
            PathCmd::Circle { c, r, a0, sweep } => {
                let pts = arc_points(*c, [*r, *r], 0.0, *a0, *sweep);
                push_arc(
                    &mut cur,
                    &mut out,
                    pts,
                    sweep.abs() >= std::f32::consts::TAU - 1e-4,
                );
            }
            PathCmd::Ellipse {
                c,
                r,
                rot,
                a0,
                sweep,
            } => {
                let pts = arc_points(*c, *r, *rot, *a0, *sweep);
                push_arc(
                    &mut cur,
                    &mut out,
                    pts,
                    sweep.abs() >= std::f32::consts::TAU - 1e-4,
                );
            }
            PathCmd::Close => {
                if let Some(s) = &mut cur {
                    s.closed = true;
                    flush(&mut cur, &mut out);
                }
            }
        }
    }
    flush(&mut cur, &mut out);
    out
}

fn push_arc(cur: &mut Option<Subpath>, out: &mut Vec<Subpath>, mut pts: Vec<[f32; 2]>, full: bool) {
    match cur {
        Some(s) if !s.pts.is_empty() => {
            let last = *s.pts.last().unwrap();
            if (last[0] - pts[0][0]).abs() > 1e-6 || (last[1] - pts[0][1]).abs() > 1e-6 {
                s.pts.push(pts[0]);
            }
            s.pts.extend_from_slice(&pts[1..]);
            if full {
                s.closed = true;
                if let Some(done) = cur.take() {
                    out.push(done);
                }
            }
        }
        _ => {
            if let Some(s) = cur.take() {
                if !s.pts.is_empty() {
                    out.push(s);
                }
            }
            pts.pop();
            *cur = Some(Subpath { pts, closed: full });
            if full {
                if let Some(done) = cur.take() {
                    out.push(done);
                }
            }
        }
    }
}

// ---- geometry -------------------------------------------------------------------------------

fn mul_affine(a: [f32; 6], b: [f32; 6]) -> [f32; 6] {
    [
        a[0] * b[0] + a[2] * b[1],
        a[1] * b[0] + a[3] * b[1],
        a[0] * b[2] + a[2] * b[3],
        a[1] * b[2] + a[3] * b[3],
        a[0] * b[4] + a[2] * b[5] + a[4],
        a[1] * b[4] + a[3] * b[5] + a[5],
    ]
}

fn apply_affine(m: [f32; 6], p: [f32; 2]) -> [f32; 2] {
    [
        m[0] * p[0] + m[2] * p[1] + m[4],
        m[1] * p[0] + m[3] * p[1] + m[5],
    ]
}

fn invert_affine(m: [f32; 6]) -> Option<[f32; 6]> {
    let det = m[0] * m[3] - m[1] * m[2];
    if !det.is_finite() || det.abs() < 1e-12 {
        return None;
    }
    Some([
        m[3] / det,
        -m[1] / det,
        -m[2] / det,
        m[0] / det,
        (m[2] * m[5] - m[3] * m[4]) / det,
        (m[1] * m[4] - m[0] * m[5]) / det,
    ])
}

/// Nonzero-winding point-in-path over device-space polygons (open ones implicitly closed).
fn inside_nonzero(polys: &[(Vec<[f32; 2]>, bool)], p: [f32; 2]) -> bool {
    let mut winding = 0i32;
    for (pts, _) in polys {
        if pts.len() < 2 {
            continue;
        }
        let n = pts.len();
        for i in 0..n {
            let a = pts[i];
            let b = pts[(i + 1) % n];
            if (a[1] <= p[1]) != (b[1] <= p[1]) {
                let xi = a[0] + (p[1] - a[1]) / (b[1] - a[1]) * (b[0] - a[0]);
                if xi > p[0] {
                    winding += if b[1] > a[1] { 1 } else { -1 };
                }
            }
        }
    }
    winding != 0
}

/// Distance from `p` to segment `ab` (clamped). Returns `(dist, t)` with `t` unclamped unless
/// `clamp` is set.
fn seg_dist(p: [f32; 2], a: [f32; 2], b: [f32; 2], clamp: bool) -> (f32, f32) {
    let (abx, aby) = (b[0] - a[0], b[1] - a[1]);
    let len2 = (abx * abx + aby * aby).max(1e-12);
    let mut t = ((p[0] - a[0]) * abx + (p[1] - a[1]) * aby) / len2;
    if clamp {
        t = t.clamp(0.0, 1.0);
    }
    let q = [a[0] + abx * t, a[1] + aby * t];
    (((p[0] - q[0]).powi(2) + (p[1] - q[1]).powi(2)).sqrt(), t)
}

fn point_in_triangle(p: [f32; 2], a: [f32; 2], b: [f32; 2], c: [f32; 2]) -> bool {
    let s = |p: [f32; 2], q: [f32; 2], r: [f32; 2]| {
        (q[0] - p[0]) * (r[1] - p[1]) - (r[0] - p[0]) * (q[1] - p[1])
    };
    let (d1, d2, d3) = (s(p, a, b), s(p, b, c), s(p, c, a));
    !((d1 < 0.0 || d2 < 0.0 || d3 < 0.0) && (d1 > 0.0 || d2 > 0.0 || d3 > 0.0))
}

fn stroke_hit(
    polys: &[(Vec<[f32; 2]>, bool)],
    p: [f32; 2],
    half: f32,
    cap: LineCap,
    join: LineJoin,
) -> bool {
    for (pts, closed) in polys {
        if pts.len() < 2 {
            // A lone moveTo strokes nothing; a zero-length subpath with round cap paints a dot.
            if pts.len() == 1 && cap == LineCap::Round {
                let d = ((p[0] - pts[0][0]).powi(2) + (p[1] - pts[0][1]).powi(2)).sqrt();
                if d <= half {
                    return true;
                }
            }
            continue;
        }
        let n = pts.len();
        let segs = if *closed { n } else { n - 1 };
        for i in 0..segs {
            let (a, b) = (pts[i % n], pts[(i + 1) % n]);
            // Extend open ends for square caps.
            let (mut a, mut b) = (a, b);
            if !closed && cap == LineCap::Square {
                let len = ((b[0] - a[0]).powi(2) + (b[1] - a[1]).powi(2))
                    .sqrt()
                    .max(1e-9);
                let (dx, dy) = ((b[0] - a[0]) / len * half, (b[1] - a[1]) / len * half);
                if i == 0 {
                    a = [a[0] - dx, a[1] - dy];
                }
                if i == segs - 1 {
                    b = [b[0] + dx, b[1] + dy];
                }
            }
            let (d, _) = seg_dist(p, a, b, true);
            if d <= half {
                return true;
            }
            // Round caps: discs on open ends.
            if !closed && cap == LineCap::Round && (i == 0 || i == segs - 1) {
                let e = if i == 0 { pts[0] } else { *pts.last().unwrap() };
                let d = ((p[0] - e[0]).powi(2) + (p[1] - e[1]).powi(2)).sqrt();
                if d <= half {
                    return true;
                }
            }
        }
        // Joins at interior vertices (and every vertex when closed).
        let verts: Vec<usize> = if *closed {
            (0..n).collect()
        } else {
            (1..n - 1).collect()
        };
        for &i in &verts {
            let (prev, v, next) = (pts[(i + n - 1) % n], pts[i], pts[(i + 1) % n]);
            if join == LineJoin::Round {
                let d = ((p[0] - v[0]).powi(2) + (p[1] - v[1]).powi(2)).sqrt();
                if d <= half {
                    return true;
                }
                continue;
            }
            // Miter / bevel: offset points of both incident edges.
            let off = |a: [f32; 2], b: [f32; 2]| {
                let len = ((b[0] - a[0]).powi(2) + (b[1] - a[1]).powi(2))
                    .sqrt()
                    .max(1e-9);
                [-(b[1] - a[1]) / len * half, (b[0] - a[0]) / len * half]
            };
            let (o1, o2) = (off(prev, v), off(v, next));
            // Outside of the bend is the side the two offsets agree on; pick the pair with the
            // larger separation (works for both turn directions).
            let (p1a, p1b) = ([v[0] + o1[0], v[1] + o1[1]], [v[0] - o1[0], v[1] - o1[1]]);
            let (p2a, p2b) = ([v[0] + o2[0], v[1] + o2[1]], [v[0] - o2[0], v[1] - o2[1]]);
            let use_a = (p1a[0] - p2a[0]).powi(2) + (p1a[1] - p2a[1]).powi(2)
                > (p1b[0] - p2b[0]).powi(2) + (p1b[1] - p2b[1]).powi(2);
            let (q1, q2) = if use_a { (p1a, p2a) } else { (p1b, p2b) };
            if join == LineJoin::Miter {
                // Intersection of the two offset lines.
                let d = (v[0] - prev[0]) * (next[1] - v[1]) - (v[1] - prev[1]) * (next[0] - v[0]);
                if d.abs() > 1e-9 {
                    let t = ((q2[0] - q1[0]) * (q1[1] - prev[1] + o1[1])
                        - (q2[1] - q1[1]) * (q1[0] - prev[0] + o1[0]))
                        / d;
                    let _ = t;
                    // Solve properly: offset line 1: prev+o1 + s*(v-prev); offset line 2: v+o2 + t*(next-v).
                    let (e1x, e1y) = (v[0] - prev[0], v[1] - prev[1]);
                    let (e2x, e2y) = (next[0] - v[0], next[1] - v[1]);
                    let denom = e1x * e2y - e1y * e2x;
                    if denom.abs() > 1e-9 {
                        let (rx, ry) = (
                            v[0] + o2[0] - (prev[0] + o1[0]),
                            v[1] + o2[1] - (prev[1] + o1[1]),
                        );
                        let s = (rx * e2y - ry * e2x) / denom;
                        let m = [prev[0] + o1[0] + e1x * s, prev[1] + o1[1] + e1y * s];
                        let miter_len =
                            ((m[0] - v[0]).powi(2) + (m[1] - v[1]).powi(2)).sqrt() / half.max(1e-9);
                        if miter_len <= MITER_LIMIT && point_in_triangle(p, v, q1, m) {
                            return true;
                        }
                        if miter_len <= MITER_LIMIT && point_in_triangle(p, v, m, q2) {
                            return true;
                        }
                        if miter_len <= MITER_LIMIT {
                            continue;
                        }
                    }
                }
            }
            // Bevel (and miter-limit fallback).
            if point_in_triangle(p, v, q1, q2) {
                return true;
            }
        }
    }
    false
}

fn gradient_at(stops: &[(f32, Color)], t: f32) -> [f32; 4] {
    if stops.is_empty() {
        return [0.0, 0.0, 0.0, 0.0];
    }
    if t <= stops[0].0 {
        let c = stops[0].1;
        return [c.r, c.g, c.b, c.a];
    }
    for w in stops.windows(2) {
        let ((t0, c0), (t1, c1)) = (w[0], w[1]);
        if t <= t1 {
            let k = if t1 > t0 { (t - t0) / (t1 - t0) } else { 1.0 };
            return [
                c0.r + (c1.r - c0.r) * k,
                c0.g + (c1.g - c0.g) * k,
                c0.b + (c1.b - c0.b) * k,
                c0.a + (c1.a - c0.a) * k,
            ];
        }
    }
    let c = stops.last().unwrap().1;
    [c.r, c.g, c.b, c.a]
}

/// Source-over blend of straight-alpha `src` (alpha already includes `globalAlpha`).
fn blend(dst: &mut [u8], r: f32, g: f32, b: f32, a: f32) {
    let a = a.clamp(0.0, 1.0);
    if a <= 0.0 {
        return;
    }
    let (dr, dg, db, da) = (
        dst[0] as f32 / 255.0,
        dst[1] as f32 / 255.0,
        dst[2] as f32 / 255.0,
        dst[3] as f32 / 255.0,
    );
    let oa = a + da * (1.0 - a);
    if oa <= 0.0 {
        dst.copy_from_slice(&[0, 0, 0, 0]);
        return;
    }
    dst[0] = ((r * a + dr * da * (1.0 - a)) / oa * 255.0)
        .round()
        .clamp(0.0, 255.0) as u8;
    dst[1] = ((g * a + dg * da * (1.0 - a)) / oa * 255.0)
        .round()
        .clamp(0.0, 255.0) as u8;
    dst[2] = ((b * a + db * da * (1.0 - a)) / oa * 255.0)
        .round()
        .clamp(0.0, 255.0) as u8;
    dst[3] = (oa * 255.0).round().clamp(0.0, 255.0) as u8;
}

// ---- CSS color parsing (subset) -----------------------------------------------------------------

/// Parse the color syntax this context accepts: `#rgb`/`#rgba`/`#rrggbb`/`#rrggbbaa`,
/// `rgb()`/`rgba()` (numbers or percentages), and the named colors below. Anything else
/// (`hsl()`, `lab()`, `color()`, system colors, …) is `None` — see the unsupported list.
pub fn parse_color(s: &str) -> Option<Color> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    if let Some(hex) = s.strip_prefix('#') {
        let d = |c: char| c.to_digit(16).map(|v| v as f32 / 15.0);
        let b = |a: char, b: char| {
            u8::from_str_radix(&format!("{a}{b}"), 16)
                .ok()
                .map(|v| v as f32 / 255.0)
        };
        let ch: Vec<char> = hex.chars().collect();
        return match ch.len() {
            3 => Some(Color::rgba(d(ch[0])?, d(ch[1])?, d(ch[2])?, 1.0)),
            4 => Some(Color::rgba(d(ch[0])?, d(ch[1])?, d(ch[2])?, d(ch[3])?)),
            6 => Some(Color::rgba(
                b(ch[0], ch[1])?,
                b(ch[2], ch[3])?,
                b(ch[4], ch[5])?,
                1.0,
            )),
            8 => Some(Color::rgba(
                b(ch[0], ch[1])?,
                b(ch[2], ch[3])?,
                b(ch[4], ch[5])?,
                b(ch[6], ch[7])?,
            )),
            _ => None,
        };
    }
    let low = s.to_ascii_lowercase();
    if let Some(inner) = low
        .strip_prefix("rgba(")
        .or_else(|| low.strip_prefix("rgb("))
    {
        let inner = inner.strip_suffix(')')?;
        let parts: Vec<&str> = inner.split(',').map(str::trim).collect();
        if parts.len() != 3 && parts.len() != 4 {
            return None;
        }
        let comp = |v: &str| {
            if let Some(p) = v.strip_suffix('%') {
                p.trim().parse::<f32>().ok().map(|n| n / 100.0)
            } else {
                v.parse::<f32>().ok().map(|n| n / 255.0)
            }
        };
        let a = match parts.get(3) {
            None => 1.0,
            Some(v) => {
                if let Some(p) = v.strip_suffix('%') {
                    p.trim().parse::<f32>().ok()? / 100.0
                } else {
                    v.parse::<f32>().ok()?
                }
            }
        };
        // Space-separated `rgb(1 2 3 / .5)` is not in the subset.
        if parts.iter().any(|p| p.contains(' ')) {
            return None;
        }
        return Some(Color::rgba(
            comp(parts[0])?,
            comp(parts[1])?,
            comp(parts[2])?,
            a.clamp(0.0, 1.0),
        ));
    }
    Some(match low.as_str() {
        "transparent" => Color::TRANSPARENT,
        "black" => Color::BLACK,
        "white" => Color::WHITE,
        "red" => Color::from_u8(255, 0, 0, 1.0),
        "lime" => Color::from_u8(0, 255, 0, 1.0),
        "blue" => Color::from_u8(0, 0, 255, 1.0),
        "yellow" => Color::from_u8(255, 255, 0, 1.0),
        "cyan" | "aqua" => Color::from_u8(0, 255, 255, 1.0),
        "magenta" | "fuchsia" => Color::from_u8(255, 0, 255, 1.0),
        "gray" | "grey" => Color::from_u8(128, 128, 128, 1.0),
        "silver" => Color::from_u8(192, 192, 192, 1.0),
        "maroon" => Color::from_u8(128, 0, 0, 1.0),
        "olive" => Color::from_u8(128, 128, 0, 1.0),
        "green" => Color::from_u8(0, 128, 0, 1.0),
        "purple" => Color::from_u8(128, 0, 128, 1.0),
        "teal" => Color::from_u8(0, 128, 128, 1.0),
        "navy" => Color::from_u8(0, 0, 128, 1.0),
        "orange" => Color::from_u8(255, 165, 0, 1.0),
        "pink" => Color::from_u8(255, 192, 203, 1.0),
        "brown" => Color::from_u8(165, 42, 42, 1.0),
        _ => return None,
    })
}

pub fn base64_encode(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let n = (chunk[0] as u32) << 16
            | (*chunk.get(1).unwrap_or(&0) as u32) << 8
            | *chunk.get(2).unwrap_or(&0) as u32;
        out.push(T[(n >> 18) as usize & 63] as char);
        out.push(T[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 {
            T[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            T[n as usize & 63] as char
        } else {
            '='
        });
    }
    out
}

// ---- cross-frame store --------------------------------------------------------------------------
//
// The store lives here (not in `DomHost`/`Engine`, which other agents own) keyed by the hosting
// document's address — stable for the engine's lifetime — plus the canvas `NodeId`, so two
// windows never share a bitmap. `PENDING` is the invalidation set: draws add, `paint.rs` drains
// per composited canvas, so a canvas draw never restyles/relayouts siblings (only
// `doc.dirty.paint` is set, which skips the relayout branch in `Engine::update`).

type StoreKey = (usize, u32);

fn store() -> &'static Mutex<HashMap<StoreKey, Canvas2D>> {
    static S: OnceLock<Mutex<HashMap<StoreKey, Canvas2D>>> = OnceLock::new();
    S.get_or_init(|| Mutex::new(HashMap::new()))
}

fn pending() -> &'static Mutex<HashSet<StoreKey>> {
    static S: OnceLock<Mutex<HashSet<StoreKey>>> = OnceLock::new();
    S.get_or_init(|| Mutex::new(HashSet::new()))
}

/// Run `f` on the canvas for `(doc, node)`, creating a `width`×`height` bitmap if needed.
pub fn with_canvas<R>(
    doc: usize,
    node: u32,
    width: u32,
    height: u32,
    f: impl FnOnce(&mut Canvas2D) -> R,
) -> R {
    let mut s = store().lock().unwrap();
    let c = s
        .entry((doc, node))
        .or_insert_with(|| Canvas2D::new(width, height));
    if c.size() != (width.min(MAX_CANVAS_DIM), height.min(MAX_CANVAS_DIM)) {
        c.resize(width, height);
    }
    f(c)
}

/// Read-only access (uploads, snapshots).
pub fn read_canvas<T>(doc: usize, node: u32, f: impl FnOnce(Option<&Canvas2D>) -> T) -> T {
    let s = store().lock().unwrap();
    f(s.get(&(doc, node)))
}

/// Mark a canvas dirty: adds to the pending set (drained by `paint.rs`) — the only invalidation
/// a canvas draw performs.
pub fn mark_dirty(doc: usize, node: u32) {
    pending().lock().unwrap().insert((doc, node));
}

/// Called from `paint.rs` after a canvas is composited into the display list.
pub fn note_painted(doc: usize, node: u32) {
    pending().lock().unwrap().remove(&(doc, node));
}

/// Canvases with undisplayed draws for this document.
pub fn pending_for(doc: usize) -> Vec<u32> {
    pending()
        .lock()
        .unwrap()
        .iter()
        .filter(|(d, _)| *d == doc)
        .map(|(_, n)| *n)
        .collect()
}

/// Drop bitmaps whose nodes are gone (checked under the caller's document lock).
pub fn prune(doc: usize, is_alive: impl Fn(u32) -> bool) {
    store()
        .lock()
        .unwrap()
        .retain(|(d, n), _| *d != doc || is_alive(*n));
    pending()
        .lock()
        .unwrap()
        .retain(|(d, n)| *d != doc || is_alive(*n));
}

/// One upload per canvas with undischarged pixels. Clears dirty flags.
pub struct CanvasUpload {
    pub node: u32,
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
    pub dirty: Option<[u32; 4]>,
}

pub fn take_uploads(doc: usize) -> Vec<CanvasUpload> {
    let mut s = store().lock().unwrap();
    let mut out = Vec::new();
    for ((d, n), c) in s.iter_mut() {
        if *d != doc {
            continue;
        }
        if let Some(r) = c.take_dirty() {
            out.push(CanvasUpload {
                node: *n,
                width: c.width,
                height: c.height,
                rgba: c.pixels.clone(),
                dirty: Some(r),
            });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn px(c: &Canvas2D, x: u32, y: u32) -> [u8; 4] {
        let i = (y * c.width + x) as usize * 4;
        [
            c.pixels[i],
            c.pixels[i + 1],
            c.pixels[i + 2],
            c.pixels[i + 3],
        ]
    }

    #[test]
    fn fill_rect_paints_exact_pixels() {
        let mut c = Canvas2D::new(16, 16);
        c.set_fill(Paint::Solid(parse_color("#ff0000").unwrap()));
        c.fill_rect(2.0, 3.0, 4.0, 5.0);
        assert_eq!(px(&c, 0, 0), [0, 0, 0, 0]);
        assert_eq!(px(&c, 2, 3), [255, 0, 0, 255]);
        assert_eq!(px(&c, 5, 7), [255, 0, 0, 255]);
        assert_eq!(px(&c, 6, 3), [0, 0, 0, 0]);
        assert_eq!(px(&c, 2, 8), [0, 0, 0, 0]);
    }

    #[test]
    fn clear_rect_restores_transparency() {
        let mut c = Canvas2D::new(8, 8);
        c.set_fill(Paint::Solid(Color::WHITE));
        c.fill_rect(0.0, 0.0, 8.0, 8.0);
        c.clear_rect(2.0, 2.0, 4.0, 4.0);
        assert_eq!(px(&c, 0, 0), [255, 255, 255, 255]);
        assert_eq!(px(&c, 4, 4), [0, 0, 0, 0]);
    }

    #[test]
    fn global_alpha_blends_source_over() {
        let mut c = Canvas2D::new(4, 4);
        c.set_fill(Paint::Solid(parse_color("blue").unwrap()));
        c.fill_rect(0.0, 0.0, 4.0, 4.0);
        c.set_fill(Paint::Solid(parse_color("red").unwrap()));
        c.set_alpha(0.5);
        c.fill_rect(0.0, 0.0, 4.0, 4.0);
        let p = px(&c, 1, 1);
        assert!(
            (p[0] as i32 - 128).abs() <= 2 && p[2] >= 126 && p[2] <= 130,
            "{p:?}"
        );
        assert_eq!(p[3], 255);
        // Out-of-range alpha is ignored, not clamped.
        c.set_alpha(7.0);
        assert_eq!(c.alpha, 0.5);
    }

    #[test]
    fn path_fill_uses_nonzero_winding() {
        let mut c = Canvas2D::new(20, 20);
        c.set_fill(Paint::Solid(Color::BLACK));
        c.begin_path();
        c.move_to(2.0, 2.0);
        c.line_to(18.0, 2.0);
        c.line_to(10.0, 18.0);
        c.close_path();
        c.fill();
        assert_eq!(px(&c, 10, 6), [0, 0, 0, 255]);
        assert_eq!(px(&c, 1, 15), [0, 0, 0, 0]);
    }

    #[test]
    fn circle_arc_fills_disc() {
        let mut c = Canvas2D::new(24, 24);
        c.set_fill(Paint::Solid(parse_color("lime").unwrap()));
        c.begin_path();
        c.arc(12.0, 12.0, 8.0, 0.0, std::f32::consts::TAU, false)
            .unwrap();
        c.fill();
        assert_eq!(px(&c, 12, 12), [0, 255, 0, 255]);
        assert_eq!(px(&c, 0, 0), [0, 0, 0, 0]);
        assert!(c.arc(0.0, 0.0, -1.0, 0.0, 1.0, false).is_err());
    }

    #[test]
    fn translate_and_scale_move_paint() {
        let mut c = Canvas2D::new(20, 20);
        c.set_fill(Paint::Solid(Color::BLACK));
        c.transform([2.0, 0.0, 0.0, 2.0, 4.0, 4.0]);
        c.fill_rect(0.0, 0.0, 4.0, 4.0);
        assert_eq!(px(&c, 3, 3), [0, 0, 0, 0]);
        assert_eq!(px(&c, 5, 5), [0, 0, 0, 255]);
        assert_eq!(px(&c, 11, 11), [0, 0, 0, 255]);
        assert_eq!(px(&c, 12, 12), [0, 0, 0, 0]);
        // save/restore round-trips the CTM but not the path.
        c.save();
        c.transform([1.0, 0.0, 0.0, 1.0, 100.0, 0.0]);
        c.restore();
        c.begin_path();
        c.rect(0.0, 0.0, 1.0, 1.0);
        c.fill();
        assert_eq!(
            px(&c, 4, 4),
            [0, 0, 0, 255],
            "restored CTM paints at the pre-save offset"
        );
    }

    #[test]
    fn stroke_centers_on_the_path_with_caps() {
        let mut c = Canvas2D::new(20, 10);
        c.set_stroke(Paint::Solid(Color::BLACK));
        c.set_line_width(4.0);
        c.begin_path();
        c.move_to(2.0, 5.0);
        c.line_to(18.0, 5.0);
        c.stroke();
        assert_eq!(px(&c, 10, 5), [0, 0, 0, 255]);
        assert_eq!(
            px(&c, 10, 2),
            [0, 0, 0, 0],
            "butt caps stay inside the vertical band"
        );
        // Round caps extend past the endpoint.
        let mut r = Canvas2D::new(20, 10);
        r.set_stroke(Paint::Solid(Color::BLACK));
        r.set_line_width(4.0);
        r.set_line_cap(LineCap::Round);
        r.begin_path();
        r.move_to(4.0, 5.0);
        r.line_to(16.0, 5.0);
        r.stroke();
        assert_eq!(
            px(&r, 2, 5),
            [0, 0, 0, 255],
            "round cap reaches back past x=4"
        );
    }

    #[test]
    fn linear_gradient_interpolates_two_stops() {
        let mut c = Canvas2D::new(64, 4);
        c.set_fill(Paint::Linear(LinearGradient {
            x0: 0.0,
            y0: 0.0,
            x1: 64.0,
            y1: 0.0,
            stops: vec![(0.0, Color::BLACK), (1.0, Color::WHITE)],
        }));
        c.fill_rect(0.0, 0.0, 64.0, 4.0);
        let (a, b, d) = (px(&c, 1, 2)[0], px(&c, 32, 2)[0], px(&c, 62, 2)[0]);
        assert!(a < 24, "{a}");
        assert!((b as i32 - 130).abs() < 14, "{b}");
        assert!(d > 230, "{d}");
    }

    #[test]
    fn draw_image_copies_canvas_pixels() {
        let mut src = Canvas2D::new(4, 4);
        src.set_fill(Paint::Solid(parse_color("red").unwrap()));
        src.fill_rect(0.0, 0.0, 4.0, 4.0);
        let bits = src.pixels.clone();
        let mut dst = Canvas2D::new(8, 8);
        dst.draw_image(&bits, 4, 4, 0.0, 0.0, 4.0, 4.0, 2.0, 2.0, 4.0, 4.0)
            .unwrap();
        assert_eq!(px(&dst, 0, 0), [0, 0, 0, 0]);
        assert_eq!(px(&dst, 3, 3), [255, 0, 0, 255]);
        assert_eq!(px(&dst, 5, 5), [255, 0, 0, 255]);
    }

    #[test]
    fn image_data_round_trips() {
        let mut c = Canvas2D::new(6, 6);
        c.set_fill(Paint::Solid(parse_color("#123456").unwrap()));
        c.fill_rect(1.0, 1.0, 2.0, 2.0);
        let data = c.get_data(0, 0, 6, 6);
        assert_eq!(data.len(), 6 * 6 * 4);
        let mut d = Canvas2D::new(6, 6);
        d.put_data(&data, 6, 6, 0, 0);
        assert_eq!(d.pixels, c.pixels);
        // Out-of-bounds reads are transparent, not an error.
        assert_eq!(c.get_data(-4, -4, 2, 2), vec![0u8; 16]);
    }

    #[test]
    fn png_bytes_have_a_png_signature() {
        let mut c = Canvas2D::new(2, 2);
        c.set_fill(Paint::Solid(Color::WHITE));
        c.fill_rect(0.0, 0.0, 2.0, 2.0);
        let png = c.to_png_bytes().unwrap();
        assert_eq!(&png[..8], &[137, 80, 78, 71, 13, 10, 26, 10]);
        assert!(Canvas2D::new(0, 0).to_png_bytes().is_none());
    }

    #[test]
    fn color_subset_parses_and_rejects() {
        assert_eq!(parse_color("#f00"), Some(Color::from_u8(255, 0, 0, 1.0)));
        let half = parse_color("#ff000080").unwrap();
        assert!(
            (half.a - 128.0 / 255.0).abs() < 1e-6 && half.r == 1.0,
            "{half:?}"
        );
        assert_eq!(
            parse_color("rgba(0, 255, 0, 0.5)").map(|c| (c.g, c.a)),
            Some((1.0, 0.5))
        );
        assert_eq!(parse_color("transparent"), Some(Color::TRANSPARENT));
        assert!(
            parse_color("hsl(0, 100%, 50%)").is_none(),
            "hsl is out of subset"
        );
        assert!(parse_color("notacolor").is_none());
    }

    #[test]
    fn store_and_pending_track_one_canvas() {
        with_canvas(0xC0FFEE, 7, 4, 4, |c| {
            c.set_fill(Paint::Solid(Color::BLACK));
            c.fill_rect(0.0, 0.0, 4.0, 4.0);
            mark_dirty(0xC0FFEE, 7);
        });
        assert_eq!(pending_for(0xC0FFEE), vec![7]);
        note_painted(0xC0FFEE, 7);
        assert!(pending_for(0xC0FFEE).is_empty());
        let ups = take_uploads(0xC0FFEE);
        assert_eq!(ups.len(), 1);
        assert_eq!((ups[0].width, ups[0].height), (4, 4));
        assert!(
            take_uploads(0xC0FFEE).is_empty(),
            "dirty flags are discharged by the upload"
        );
        prune(0xC0FFEE, |_| false);
        read_canvas(0xC0FFEE, 7, |c| assert!(c.is_none()));
    }
}
