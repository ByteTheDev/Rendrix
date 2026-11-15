//! Minimal inline/file SVG rasterizer (approved subset).
//!
//! Renders `<svg>` content to a straight-alpha RGBA bitmap on the CPU, then hands it to the
//! existing texture-upload path (`Renderer::upload_image_as` for files, the same image cache the
//! `<img>` paint path already uses). No new media dependencies.
//!
//! ## Why hand-rolled instead of tiny-skia / resvg
//! The only missing pieces were (a) an SVG *parser* and (b) a *rasterizer*. Pulling in
//! `tiny-skia` would still leave (a) unsolved (it is a 2D canvas, not an SVG parser; a full
//! parser is `usvg`/`resvg`, which drags in font shaping, text layout and system libraries).
//! The approved subset (flat shapes + solid fills + simple strokes, no text, no gradients,
//! no filters, no scripting) is ~500 lines of scanline raster code that deliberately mirrors
//! `canvas.rs` conventions (hard 50% coverage test, source-over blending, straight-alpha
//! RGBA), so behavior is consistent with the Canvas2D subset and works on every target
//! (including wasm) with zero new dependencies and no binary-size cliff.
//!
//! ## Approved subset
//! Elements: `svg`, `rect` (incl. `rx`/`ry`), `circle`, `ellipse`, `line`, `polyline`,
//! `polygon`, `path` (commands `M L H V C S Q T Z`, absolute + relative; `A` arcs are
//! straight-lined to their endpoint), `g` (attribute inheritance only). Presentation
//! attributes: `fill` (`none` or any CSS color the engine parses), `stroke`, `stroke-width`,
//! `opacity`, `fill-opacity`, `stroke-opacity`, `fill-rule` (`nonzero`|`evenodd`),
//! `transform` (`translate`/`scale`/`rotate`, applied to geometry). Root: `width`/`height`
//! (px numbers) and `viewBox` with the default `preserveAspectRatio` (`xMidYMid meet`;
//! `none` stretches). Everything else (`<text>`, gradients, filters, masks, markers,
//! `stroke-dasharray`, percentage lengths, external refs, CSS in `<style>`) is ignored and
//! documented as such in COMPATIBILITY.md.

use rendrix_css::Color;

const MAX_DIM: u32 = 2048;
const DEFAULT_W: f32 = 300.0;
const DEFAULT_H: f32 = 150.0;

#[derive(Debug, Clone)]
struct Inherited {
    fill: Option<Color>,
    stroke: Option<Color>,
    stroke_width: f32,
    opacity: f32,
    fill_opacity: f32,
    stroke_opacity: f32,
    evenodd: bool,
    transform: [f32; 6],
}

impl Default for Inherited {
    fn default() -> Self {
        Inherited {
            fill: Some(Color::BLACK),
            stroke: None,
            stroke_width: 1.0,
            opacity: 1.0,
            fill_opacity: 1.0,
            stroke_opacity: 1.0,
            evenodd: false,
            transform: [1.0, 0.0, 0.0, 1.0, 0.0, 0.0],
        }
    }
}

#[derive(Debug, Clone)]
struct Shape {
    polys: Vec<Vec<[f32; 2]>>,
    closed_flags: Vec<bool>,
    fill: Option<Color>,
    stroke: Option<Color>,
    stroke_width: f32,
    evenodd: bool,
}

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
    [m[0] * p[0] + m[2] * p[1] + m[4], m[1] * p[0] + m[3] * p[1] + m[5]]
}

// ---- attribute helpers ---------------------------------------------------------------

type Attrs = Vec<(String, String)>;

/// Parse `name="value"` / `name='value'` pairs from a tag interior (everything between
/// `<tag` and `>`). Malformed pairs are skipped, never fatal.
fn parse_attrs(tag: &str) -> Attrs {
    let mut out = Vec::new();
    let b = tag.as_bytes();
    let mut i = 0;
    while i < b.len() {
        while i < b.len() && (b[i].is_ascii_whitespace() || b[i] == b'/') {
            i += 1;
        }
        if i >= b.len() {
            break;
        }
        let ns = i;
        while i < b.len() && !b[i].is_ascii_whitespace() && b[i] != b'=' && b[i] != b'/' && b[i] != b'>' {
            i += 1;
        }
        if ns == i {
            i += 1;
            continue;
        }
        let name = tag[ns..i].to_ascii_lowercase();
        while i < b.len() && b[i].is_ascii_whitespace() {
            i += 1;
        }
        if i >= b.len() || b[i] != b'=' {
            continue;
        }
        i += 1;
        while i < b.len() && b[i].is_ascii_whitespace() {
            i += 1;
        }
        if i >= b.len() {
            break;
        }
        let (val, next) = if b[i] == b'"' || b[i] == b'\'' {
            let q = b[i];
            let mut j = i + 1;
            while j < b.len() && b[j] != q {
                j += 1;
            }
            let v = tag[i + 1..j.min(b.len())].to_string();
            (v, j.saturating_add(1))
        } else {
            let mut j = i;
            while j < b.len() && !b[j].is_ascii_whitespace() && b[j] != b'>' {
                j += 1;
            }
            (tag[i..j].to_string(), j)
        };
        out.push((name, val));
        i = next;
    }
    out
}

fn attr<'a>(attrs: &'a Attrs, name: &str) -> Option<&'a str> {
    attrs.iter().find(|(k, _)| k == name).map(|(_, v)| v.as_str())
}

fn parse_num(s: &str) -> Option<f32> {
    let s = s.trim().trim_end_matches("px");
    s.parse::<f32>().ok().filter(|v| v.is_finite())
}

fn parse_opt_num(attrs: &Attrs, name: &str, def: f32) -> f32 {
    attr(attrs, name).and_then(parse_num).unwrap_or(def)
}

/// CSS color or `none`. `currentColor` resolves to black (no CSS cascade inside SVG).
fn parse_paint(s: &str) -> Option<Color> {
    let t = s.trim();
    if t.eq_ignore_ascii_case("none") {
        return None;
    }
    Color::parse(t, Color::BLACK)
}

/// Fold `style="fill:...;stroke:...;..."` presentation declarations into the attribute list.
fn expand_style(attrs: &mut Attrs) {
    let Some(pos) = attrs.iter().position(|(k, _)| k == "style") else { return };
    let style = attrs.remove(pos).1;
    for decl in style.split(';') {
        let Some((k, v)) = decl.split_once(':') else { continue };
        let k = k.trim().to_ascii_lowercase();
        let v = v.trim().to_string();
        if matches!(k.as_str(), "fill" | "stroke" | "stroke-width" | "opacity" | "fill-opacity" | "stroke-opacity" | "fill-rule")
            && !attrs.iter().any(|(ek, _)| *ek == k)
        {
            attrs.push((k, v));
        }
    }
}

fn parse_transform(s: &str) -> [f32; 6] {
    let mut m = [1.0, 0.0, 0.0, 1.0, 0.0, 0.0];
    let mut rest = s.trim();
    while !rest.is_empty() {
        rest = rest.trim_start();
        let (name, after) = match rest.find('(') {
            Some(i) => (rest[..i].trim().to_ascii_lowercase(), &rest[i + 1..]),
            None => break,
        };
        let Some(end) = after.find(')') else { break };
        let args: Vec<f32> = after[..end]
            .split(|c: char| c == ',' || c.is_whitespace())
            .filter(|p| !p.is_empty())
            .filter_map(|p| p.parse::<f32>().ok().filter(|v| v.is_finite()))
            .collect();
        let t = match name.as_str() {
            "translate" => {
                let (tx, ty) = (args.first().copied().unwrap_or(0.0), args.get(1).copied().unwrap_or(0.0));
                [1.0, 0.0, 0.0, 1.0, tx, ty]
            }
            "scale" => {
                let (sx, sy) = (args.first().copied().unwrap_or(1.0), args.get(1).copied().unwrap_or(args.first().copied().unwrap_or(1.0)));
                [sx, 0.0, 0.0, sy, 0.0, 0.0]
            }
            "rotate" => {
                let a = args.first().copied().unwrap_or(0.0).to_radians();
                let (c, s) = (a.cos(), a.sin());
                if args.len() >= 3 {
                    let (cx, cy) = (args[1], args[2]);
                    mul_affine([1.0, 0.0, 0.0, 1.0, cx, cy], mul_affine([c, s, -s, c, 0.0, 0.0], [1.0, 0.0, 0.0, 1.0, -cx, -cy]))
                } else {
                    [c, s, -s, c, 0.0, 0.0]
                }
            }
            "matrix" if args.len() >= 6 => [args[0], args[1], args[2], args[3], args[4], args[5]],
            _ => {
                rest = after[end + 1..].trim();
                continue;
            }
        };
        m = mul_affine(m, t);
        rest = after[end + 1..].trim();
        if rest.starts_with(',') {
            rest = &rest[1..];
        }
    }
    m
}

fn inherit_from(attrs: &Attrs, parent: &Inherited) -> Inherited {
    let mut h = parent.clone();
    if let Some(v) = attr(attrs, "fill") {
        h.fill = parse_paint(v);
    }
    if let Some(v) = attr(attrs, "stroke") {
        h.stroke = parse_paint(v);
    }
    if let Some(v) = attr(attrs, "stroke-width") {
        if let Some(n) = parse_num(v) {
            h.stroke_width = n.max(0.0);
        }
    }
    if let Some(v) = attr(attrs, "opacity") {
        if let Ok(n) = v.trim().parse::<f32>() {
            if n.is_finite() {
                h.opacity = n.clamp(0.0, 1.0);
            }
        }
    }
    if let Some(v) = attr(attrs, "fill-opacity") {
        if let Ok(n) = v.trim().parse::<f32>() {
            if n.is_finite() {
                h.fill_opacity = n.clamp(0.0, 1.0);
            }
        }
    }
    if let Some(v) = attr(attrs, "stroke-opacity") {
        if let Ok(n) = v.trim().parse::<f32>() {
            if n.is_finite() {
                h.stroke_opacity = n.clamp(0.0, 1.0);
            }
        }
    }
    if let Some(v) = attr(attrs, "fill-rule") {
        h.evenodd = v.trim().eq_ignore_ascii_case("evenodd");
    }
    if let Some(v) = attr(attrs, "transform") {
        h.transform = mul_affine(h.transform, parse_transform(v));
    }
    h
}

// ---- shape builders ------------------------------------------------------------------

fn xf_poly(pts: &[[f32; 2]], xf: [f32; 6]) -> Vec<[f32; 2]> {
    pts.iter().map(|p| apply_affine(xf, *p)).collect()
}

fn ellipse_points(c: [f32; 2], r: [f32; 2]) -> Vec<[f32; 2]> {
    if r[0] <= 0.0 || r[1] <= 0.0 {
        return Vec::new();
    }
    let n = ((r[0].max(r[1]) * 0.7).ceil().clamp(20.0, 256.0)) as usize;
    (0..n).map(|i| {
        let a = i as f32 / n as f32 * std::f32::consts::TAU;
        [c[0] + r[0] * a.cos(), c[1] + r[1] * a.sin()]
    }).collect()
}

fn rounded_rect_points(x: f32, y: f32, w: f32, h: f32, rx: f32, ry: f32) -> Vec<[f32; 2]> {
    if w <= 0.0 || h <= 0.0 {
        return Vec::new();
    }
    let (rx, ry) = (rx.min(w / 2.0).max(0.0), ry.min(h / 2.0).max(0.0));
    if rx <= 0.0 || ry <= 0.0 {
        return vec![[x, y], [x + w, y], [x + w, y + h], [x, y + h]];
    }
    let mut pts = Vec::new();
    let corners = [
        ([x + w - rx, y + ry], rx, ry, -std::f32::consts::FRAC_PI_2, 0.0),
        ([x + w - rx, y + h - ry], rx, ry, 0.0, std::f32::consts::FRAC_PI_2),
        ([x + rx, y + h - ry], rx, ry, std::f32::consts::FRAC_PI_2, std::f32::consts::PI),
        ([x + rx, y + ry], rx, ry, std::f32::consts::PI, 3.0 * std::f32::consts::FRAC_PI_2),
    ];
    for (c, crx, cry, a0, a1) in corners {
        let n = 8;
        for i in 0..=n {
            let a = a0 + (a1 - a0) * i as f32 / n as f32;
            pts.push([c[0] + crx * a.cos(), c[1] + cry * a.sin()]);
        }
    }
    pts
}

// ---- path data ------------------------------------------------------------------------

fn path_tokenize(d: &str) -> Vec<String> {
    let mut toks = Vec::new();
    let mut cur = String::new();
    let mut chars = d.chars().peekable();
    while let Some(c) = chars.next() {
        if c.is_ascii_alphabetic() {
            if !cur.is_empty() {
                toks.push(std::mem::take(&mut cur));
            }
            toks.push(c.to_string());
        } else if c == ',' || c.is_whitespace() {
            if !cur.is_empty() {
                toks.push(std::mem::take(&mut cur));
            }
        } else if c == '-' || c == '+' {
            if !cur.is_empty()
                && !cur.ends_with('e')
                && !cur.ends_with('E')
            {
                toks.push(std::mem::take(&mut cur));
            }
            cur.push(c);
        } else if c == '.' {
            if cur.contains('.') {
                toks.push(std::mem::take(&mut cur));
            }
            cur.push(c);
        } else {
            cur.push(c);
        }
    }
    if !cur.is_empty() {
        toks.push(cur);
    }
    toks
}

fn cubic_at(p0: [f32; 2], p1: [f32; 2], p2: [f32; 2], p3: [f32; 2], t: f32) -> [f32; 2] {
    let u = 1.0 - t;
    [
        u * u * u * p0[0] + 3.0 * u * u * t * p1[0] + 3.0 * u * t * t * p2[0] + t * t * t * p3[0],
        u * u * u * p0[1] + 3.0 * u * u * t * p1[1] + 3.0 * u * t * t * p2[1] + t * t * t * p3[1],
    ]
}

fn quad_at(p0: [f32; 2], p1: [f32; 2], p2: [f32; 2], t: f32) -> [f32; 2] {
    let u = 1.0 - t;
    [u * u * p0[0] + 2.0 * u * t * p1[0] + t * t * p2[0], u * u * p0[1] + 2.0 * u * t * p1[1] + t * t * p2[1]]
}

/// Flatten path data into subpaths (each a polyline + closed flag). Unsupported commands
/// (`A`) are degraded to straight lines to their endpoints so content still appears.
fn next_num(toks: &[String], i: &mut usize) -> Option<f32> {
    if *i < toks.len() {
        if let Ok(n) = toks[*i].parse::<f32>() {
            if n.is_finite() {
                *i += 1;
                return Some(n);
            }
        }
    }
    None
}

fn flatten_path(d: &str) -> Vec<(Vec<[f32; 2]>, bool)> {
    let toks = path_tokenize(d);
    let mut out: Vec<(Vec<[f32; 2]>, bool)> = Vec::new();
    let mut cur: Vec<[f32; 2]> = Vec::new();
    let mut closed = false;
    let mut pos = [0.0f32, 0.0];
    let mut start = [0.0f32, 0.0];
    let mut cmd = 'M';
    let mut i = 0;
    let flush = |cur: &mut Vec<[f32; 2]>, closed: &mut bool, out: &mut Vec<(Vec<[f32; 2]>, bool)>| {
        if !cur.is_empty() {
            out.push((std::mem::take(cur), std::mem::take(closed)));
        }
    };
    while i < toks.len() {
        let t = toks[i].clone();
        if t.len() == 1 && t.chars().next().is_some_and(|c| c.is_ascii_alphabetic()) {
            cmd = t.chars().next().unwrap();
            i += 1;
            if matches!(cmd, 'Z' | 'z') {
                if !cur.is_empty() {
                    closed = true;
                    pos = start;
                    flush(&mut cur, &mut closed, &mut out);
                }
                continue;
            }
            continue;
        }
        let rel = cmd.is_ascii_lowercase();
        let up = cmd.to_ascii_uppercase();
        let pt = |x: f32, y: f32| if rel { [pos[0] + x, pos[1] + y] } else { [x, y] };
        match up {
            'M' => {
                let (Some(x), Some(y)) = (next_num(&toks, &mut i), next_num(&toks, &mut i)) else { break };
                let p = pt(x, y);
                flush(&mut cur, &mut closed, &mut out);
                cur.push(p);
                pos = p;
                start = p;
                // Subsequent implicit pairs are linetos.
                cmd = if rel { 'l' } else { 'L' };
            }
            'L' => {
                let (Some(x), Some(y)) = (next_num(&toks, &mut i), next_num(&toks, &mut i)) else { break };
                let p = pt(x, y);
                if cur.is_empty() {
                    cur.push(pos);
                }
                cur.push(p);
                pos = p;
            }
            'H' => {
                let Some(x) = next_num(&toks, &mut i) else { break };
                let p = if rel { [pos[0] + x, pos[1]] } else { [x, pos[1]] };
                if cur.is_empty() {
                    cur.push(pos);
                }
                cur.push(p);
                pos = p;
            }
            'V' => {
                let Some(y) = next_num(&toks, &mut i) else { break };
                let p = if rel { [pos[0], pos[1] + y] } else { [pos[0], y] };
                if cur.is_empty() {
                    cur.push(pos);
                }
                cur.push(p);
                pos = p;
            }
            'C' => {
                let (Some(x1), Some(y1), Some(x2), Some(y2), Some(x), Some(y)) = (next_num(&toks, &mut i), next_num(&toks, &mut i), next_num(&toks, &mut i), next_num(&toks, &mut i), next_num(&toks, &mut i), next_num(&toks, &mut i)) else { break };
                let (p1, p2, p3) = (pt(x1, y1), pt(x2, y2), pt(x, y));
                let p0 = pos;
                if cur.is_empty() {
                    cur.push(p0);
                }
                for k in 1..=12 {
                    cur.push(cubic_at(p0, p1, p2, p3, k as f32 / 12.0));
                }
                pos = p3;
            }
            'S' => {
                // Smooth cubic: first control mirrors the previous C/S second control.
                let (Some(x2), Some(y2), Some(x), Some(y)) = (next_num(&toks, &mut i), next_num(&toks, &mut i), next_num(&toks, &mut i), next_num(&toks, &mut i)) else { break };
                let p0 = pos;
                // Recover previous second control from the flattened tail (last two points).
                let p1 = if cur.len() >= 2 {
                    let (a, b) = (cur[cur.len() - 2], cur[cur.len() - 1]);
                    [2.0 * b[0] - a[0], 2.0 * b[1] - a[1]]
                } else {
                    p0
                };
                let (p2, p3) = (pt(x2, y2), pt(x, y));
                if cur.is_empty() {
                    cur.push(p0);
                }
                for k in 1..=12 {
                    cur.push(cubic_at(p0, p1, p2, p3, k as f32 / 12.0));
                }
                pos = p3;
            }
            'Q' => {
                let (Some(x1), Some(y1), Some(x), Some(y)) = (next_num(&toks, &mut i), next_num(&toks, &mut i), next_num(&toks, &mut i), next_num(&toks, &mut i)) else { break };
                let (p1, p2) = (pt(x1, y1), pt(x, y));
                let p0 = pos;
                if cur.is_empty() {
                    cur.push(p0);
                }
                for k in 1..=8 {
                    cur.push(quad_at(p0, p1, p2, k as f32 / 8.0));
                }
                pos = p2;
            }
            'T' => {
                let (Some(x), Some(y)) = (next_num(&toks, &mut i), next_num(&toks, &mut i)) else { break };
                let p2 = pt(x, y);
                let p0 = pos;
                let p1 = if cur.len() >= 2 {
                    let (a, b) = (cur[cur.len() - 2], cur[cur.len() - 1]);
                    [2.0 * b[0] - a[0], 2.0 * b[1] - a[1]]
                } else {
                    p0
                };
                if cur.is_empty() {
                    cur.push(p0);
                }
                for k in 1..=8 {
                    cur.push(quad_at(p0, p1, p2, k as f32 / 8.0));
                }
                pos = p2;
            }
            'A' => {
                // Arc: degraded to a straight line to the endpoint (documented subset gap).
                let vals: Vec<Option<f32>> = (0..7).map(|_| next_num(&toks, &mut i)).collect();
                if vals.iter().any(|v| v.is_none()) {
                    break;
                }
                let (x, y) = (vals[5].unwrap(), vals[6].unwrap());
                let p = pt(x, y);
                if cur.is_empty() {
                    cur.push(pos);
                }
                cur.push(p);
                pos = p;
            }
            _ => break,
        }
    }
    flush(&mut cur, &mut closed, &mut out);
    out
}

// ---- document model -------------------------------------------------------------------

struct SvgDoc {
    shapes: Vec<Shape>,
}

fn parse_view_box(s: &str) -> Option<[f32; 4]> {
    let nums: Vec<f32> = s
        .split(|c: char| c == ',' || c.is_whitespace())
        .filter(|p| !p.is_empty())
        .filter_map(|p| p.parse::<f32>().ok().filter(|v| v.is_finite()))
        .collect();
    if nums.len() == 4 && nums[2] > 0.0 && nums[3] > 0.0 {
        Some([nums[0], nums[1], nums[2], nums[3]])
    } else {
        None
    }
}

fn parse_root_size(bytes: &str) -> (Option<f32>, Option<f32>, Option<[f32; 4]>, bool) {
    let doc = bytes;
    let Some(start) = doc.find("<svg") else { return (None, None, None, false) };
    let after = &doc[start + 4..];
    let Some(end) = after.find('>') else { return (None, None, None, false) };
    let attrs = parse_attrs(&after[..end]);
    let num_attr = |n: &str| attr(&attrs, n).and_then(|v| {
        let v = v.trim();
        // Percentages on root width/height resolve against the viewport at layout time;
        // for image decoding they are not intrinsic, so ignore them here.
        if v.ends_with('%') {
            return None;
        }
        parse_num(v).filter(|x| *x > 0.0)
    });
    let vb = attr(&attrs, "viewbox").and_then(parse_view_box);
    let preserve_none = attr(&attrs, "preserveaspectratio").is_some_and(|v| v.trim().eq_ignore_ascii_case("none"));
    (num_attr("width"), num_attr("height"), vb, preserve_none)
}

/// Intrinsic (natural) size in px: explicit width/height win, else the viewBox size,
/// else the 300x150 replaced-element default.
pub fn intrinsic_size(bytes: &[u8]) -> Option<(u32, u32)> {
    let s = std::str::from_utf8(bytes).ok()?;
    if !s.contains("<svg") {
        return None;
    }
    let (w, h, vb, _) = parse_root_size(s);
    let (w, h) = match (w, h) {
        (Some(w), Some(h)) => (w, h),
        (Some(w), None) => {
            let ratio = vb.map(|v| v[3] / v[2]).unwrap_or(DEFAULT_H / DEFAULT_W);
            (w, w * ratio)
        }
        (None, Some(h)) => {
            let ratio = vb.map(|v| v[2] / v[3]).unwrap_or(DEFAULT_W / DEFAULT_H);
            (h * ratio, h)
        }
        (None, None) => match vb {
            Some(v) => (v[2], v[3]),
            None => (DEFAULT_W, DEFAULT_H),
        },
    };
    if !(w.is_finite() && h.is_finite() && w > 0.0 && h > 0.0) {
        return None;
    }
    Some((w.min(MAX_DIM as f32).max(1.0).round() as u32, h.min(MAX_DIM as f32).max(1.0).round() as u32))
}

pub fn is_svg_bytes(bytes: &[u8]) -> bool {
    if bytes.len() < 5 {
        return false;
    }
    let head = &bytes[..bytes.len().min(1024)];
    let Ok(s) = std::str::from_utf8(head) else { return false };
    let t = s.trim_start();
    (t.starts_with('<') && s.contains("<svg")) || t.starts_with("<?xml") && s.contains("<svg")
}

fn build_doc(s: &str, tw: f32, th: f32) -> Option<SvgDoc> {
    let (w, h, vb, preserve_none) = parse_root_size(s);
    let (vw, vh) = match (w, h) {
        (Some(w), Some(h)) => (w, h),
        (Some(w), None) => (w, w * vb.map(|v| v[3] / v[2]).unwrap_or(DEFAULT_H / DEFAULT_W)),
        (None, Some(h)) => (h * vb.map(|v| v[2] / v[3]).unwrap_or(DEFAULT_W / DEFAULT_H), h),
        (None, None) => match vb {
            Some(v) => (v[2], v[3]),
            None => (DEFAULT_W, DEFAULT_H),
        },
    };
    if !(vw.is_finite() && vh.is_finite() && vw > 0.0 && vh > 0.0) {
        return None;
    }
    // ViewBox → output transform (meet + center, or stretch for preserveAspectRatio="none").
    // When there is no viewBox, user units are px: scale intrinsic → output 1:1 unless the
    // caller asked for a different output size (uniform stretch to fit).
    let vb_xf = match vb {
        Some(v) if v[2] > 0.0 && v[3] > 0.0 => {
            if preserve_none {
                [tw / v[2], 0.0, 0.0, th / v[3], -v[0] * tw / v[2], -v[1] * th / v[3]]
            } else {
                let k = (tw / v[2]).min(th / v[3]);
                let (ox, oy) = ((tw - v[2] * k) / 2.0, (th - v[3] * k) / 2.0);
                [k, 0.0, 0.0, k, ox - v[0] * k, oy - v[1] * k]
            }
        }
        _ => [tw / vw, 0.0, 0.0, th / vh, 0.0, 0.0],
    };
    let mut doc = SvgDoc { shapes: Vec::new() };
    // Scan tags; maintain a group-inheritance stack for <g>.
    let mut stack: Vec<(Inherited, String)> = vec![(Inherited::default(), "svg".to_string())];
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let Some(open) = s[i..].find('<') else { break };
        i += open;
        let rest = &s[i..];
        if rest.starts_with("<!--") {
            if let Some(end) = rest.find("-->") {
                i += end + 3;
                continue;
            }
            break;
        }
        let Some(end) = rest.find('>') else { break };
        let tag = &rest[1..end];
        i += end + 1;
        let tag_trim = tag.trim();
        if tag_trim.starts_with('/') {
            let name = tag_trim[1..].split(|c: char| c.is_whitespace() || c == '/').next().unwrap_or("").to_ascii_lowercase();
            if name == "g" || name == "svg" {
                if stack.len() > 1 {
                    stack.pop();
                }
            }
            continue;
        }
        let mut name_end = tag_trim.len();
        for (k, c) in tag_trim.char_indices() {
            if c.is_whitespace() || c == '/' {
                name_end = k;
                break;
            }
        }
        let name = tag_trim[..name_end].to_ascii_lowercase();
        let mut attrs = parse_attrs(&tag_trim[name_end..]);
        expand_style(&mut attrs);
        let parent = stack.last().map(|(h, _)| h.clone()).unwrap_or_default();
        match name.as_str() {
            "g" | "svg" => {
                let h = inherit_from(&attrs, &parent);
                let self_closed = tag_trim.ends_with('/');
                if !self_closed {
                    stack.push((h, name));
                }
            }
            "rect" | "circle" | "ellipse" | "line" | "polyline" | "polygon" | "path" => {
                let h = inherit_from(&attrs, &parent);
                if let Some(sh) = build_shape(&name, &attrs, &h, vb_xf) {
                    doc.shapes.push(sh);
                }
            }
            _ => {}
        }
    }
    Some(doc)
}

fn shape_colors(h: &Inherited) -> (Option<Color>, Option<Color>) {
    let fill = h.fill.map(|c| Color { a: c.a * h.opacity * h.fill_opacity, ..c });
    let stroke = h.stroke.map(|c| Color { a: c.a * h.opacity * h.stroke_opacity, ..c });
    (
        fill.filter(|c| c.a > 0.0),
        stroke.filter(|c| c.a > 0.0 && h.stroke_width > 0.0),
    )
}

fn build_shape(name: &str, attrs: &Attrs, h: &Inherited, vb_xf: [f32; 6]) -> Option<Shape> {
    let xf = mul_affine(vb_xf, h.transform);
    let (fill, stroke) = shape_colors(h);
    if fill.is_none() && stroke.is_none() {
        return None;
    }
    let polys: Vec<Vec<[f32; 2]>> = match name {
        "rect" => {
            let (x, y) = (parse_opt_num(attrs, "x", 0.0), parse_opt_num(attrs, "y", 0.0));
            let (w, hh) = (parse_opt_num(attrs, "width", 0.0), parse_opt_num(attrs, "height", 0.0));
            if w <= 0.0 || hh <= 0.0 {
                return None;
            }
            let rx = parse_opt_num(attrs, "rx", 0.0);
            let ry = attr(attrs, "ry").and_then(parse_num).unwrap_or(rx);
            vec![xf_poly(&rounded_rect_points(x, y, w, hh, rx, ry), xf)]
        }
        "circle" => {
            let (cx, cy) = (parse_opt_num(attrs, "cx", 0.0), parse_opt_num(attrs, "cy", 0.0));
            let r = attr(attrs, "r").and_then(parse_num).unwrap_or(0.0);
            if r <= 0.0 {
                return None;
            }
            // Transform may be non-uniform: sample the ellipse in local space, then transform.
            let pts = ellipse_points([cx, cy], [r, r]);
            if pts.is_empty() {
                return None;
            }
            vec![xf_poly(&pts, xf)]
        }
        "ellipse" => {
            let (cx, cy) = (parse_opt_num(attrs, "cx", 0.0), parse_opt_num(attrs, "cy", 0.0));
            let (rx, ry) = (attr(attrs, "rx").and_then(parse_num).unwrap_or(0.0), attr(attrs, "ry").and_then(parse_num).unwrap_or(0.0));
            if rx <= 0.0 || ry <= 0.0 {
                return None;
            }
            let pts = ellipse_points([cx, cy], [rx, ry]);
            if pts.is_empty() {
                return None;
            }
            vec![xf_poly(&pts, xf)]
        }
        "line" => {
            let (x1, y1) = (parse_opt_num(attrs, "x1", 0.0), parse_opt_num(attrs, "y1", 0.0));
            let (x2, y2) = (parse_opt_num(attrs, "x2", 0.0), parse_opt_num(attrs, "y2", 0.0));
            if fill.is_some() {
                // Lines have no area: only strokes paint.
            }
            let p = xf_poly(&[[x1, y1], [x2, y2]], xf);
            // A line is stroke-only.
            return Some(Shape { polys: vec![p], closed_flags: vec![false], fill: None, stroke, stroke_width: h.stroke_width, evenodd: h.evenodd });
        }
        "polyline" | "polygon" => {
            let Some(pstr) = attr(attrs, "points") else { return None };
            let nums: Vec<f32> = pstr
                .split(|c: char| c == ',' || c.is_whitespace())
                .filter(|p| !p.is_empty())
                .filter_map(|p| p.parse::<f32>().ok().filter(|v| v.is_finite()))
                .collect();
            if nums.len() < 4 {
                return None;
            }
            let pts: Vec<[f32; 2]> = nums.chunks_exact(2).map(|c| [c[0], c[1]]).collect();
            vec![xf_poly(&pts, xf)]
        }
        "path" => {
            let Some(d) = attr(attrs, "d") else { return None };
            let subs = flatten_path(d);
            if subs.is_empty() {
                return None;
            }
            subs.into_iter().map(|(p, _)| xf_poly(&p, xf)).collect()
        }
        _ => return None,
    };
    if polys.iter().all(|p| p.len() < 2) {
        return None;
    }
    let closed_flags = polys
        .iter()
        .map(|_| matches!(name, "rect" | "circle" | "ellipse" | "polygon"))
        .collect();
    // Open polylines/paths fill implicitly closed (matches canvas fill semantics).
    Some(Shape { polys, closed_flags, fill, stroke, stroke_width: h.stroke_width, evenodd: h.evenodd })
}

// ---- raster ---------------------------------------------------------------------------------

fn blend_px(dst: &mut [u8], r: f32, g: f32, b: f32, a: f32) {
    if a <= 0.0 {
        return;
    }
    let a = a.clamp(0.0, 1.0);
    let (dr, dg, db, da) = (dst[0] as f32 / 255.0, dst[1] as f32 / 255.0, dst[2] as f32 / 255.0, dst[3] as f32 / 255.0);
    let out_a = a + da * (1.0 - a);
    if out_a <= 0.0 {
        dst.copy_from_slice(&[0, 0, 0, 0]);
        return;
    }
    dst[0] = (((r * a + dr * da * (1.0 - a)) / out_a) * 255.0).round().clamp(0.0, 255.0) as u8;
    dst[1] = (((g * a + dg * da * (1.0 - a)) / out_a) * 255.0).round().clamp(0.0, 255.0) as u8;
    dst[2] = (((b * a + db * da * (1.0 - a)) / out_a) * 255.0).round().clamp(0.0, 255.0) as u8;
    dst[3] = (out_a * 255.0).round().clamp(0.0, 255.0) as u8;
}

fn inside(polys: &[Vec<[f32; 2]>], p: [f32; 2], evenodd: bool) -> bool {
    if evenodd {
        let mut crossings = 0u32;
        for pts in polys {
            if pts.len() < 2 {
                continue;
            }
            for i in 0..pts.len() {
                let (a, b) = (pts[i], pts[(i + 1) % pts.len()]);
                if (a[1] <= p[1]) != (b[1] <= p[1]) {
                    let xi = a[0] + (p[1] - a[1]) / (b[1] - a[1]) * (b[0] - a[0]);
                    if xi > p[0] {
                        crossings += 1;
                    }
                }
            }
        }
        crossings % 2 == 1
    } else {
        let mut winding = 0i32;
        for pts in polys {
            if pts.len() < 2 {
                continue;
            }
            for i in 0..pts.len() {
                let (a, b) = (pts[i], pts[(i + 1) % pts.len()]);
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
}

fn seg_dist(p: [f32; 2], a: [f32; 2], b: [f32; 2]) -> f32 {
    let (abx, aby) = (b[0] - a[0], b[1] - a[1]);
    let len2 = (abx * abx + aby * aby).max(1e-12);
    let t = (((p[0] - a[0]) * abx + (p[1] - a[1]) * aby) / len2).clamp(0.0, 1.0);
    let q = [a[0] + abx * t, a[1] + aby * t];
    ((p[0] - q[0]).powi(2) + (p[1] - q[1]).powi(2)).sqrt()
}

fn stroke_inside(polys: &[Vec<[f32; 2]>], closed: &[bool], p: [f32; 2], half: f32) -> bool {
    for (pts, c) in polys.iter().zip(closed.iter()) {
        if pts.len() < 2 {
            if pts.len() == 1 {
                let d = ((p[0] - pts[0][0]).powi(2) + (p[1] - pts[0][1]).powi(2)).sqrt();
                if d <= half {
                    return true;
                }
            }
            continue;
        }
        let n = pts.len();
        let segs = if *c { n } else { n - 1 };
        for i in 0..segs {
            if seg_dist(p, pts[i % n], pts[(i + 1) % n]) <= half {
                return true;
            }
        }
        // Round caps / joins: discs on every vertex (slight overdraw at acute joins,
        // consistent with the canvas subset's round-join handling).
        for q in pts {
            let d = ((p[0] - q[0]).powi(2) + (p[1] - q[1]).powi(2)).sqrt();
            if d <= half {
                return true;
            }
        }
    }
    false
}

/// Rasterize at an explicit pixel size (scales the document's viewBox to fit).
pub fn rasterize(bytes: &[u8], width: u32, height: u32) -> Option<(u32, u32, Vec<u8>)> {
    if width == 0 || height == 0 || width > MAX_DIM || height > MAX_DIM {
        return None;
    }
    let s = std::str::from_utf8(bytes).ok()?;
    if !s.contains("<svg") {
        return None;
    }
    let doc = build_doc(s, width as f32, height as f32)?;
    let mut px = vec![0u8; (width * height * 4) as usize];
    for sh in &doc.shapes {
        let (mut x0, mut y0, mut x1, mut y1) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
        for poly in &sh.polys {
            for p in poly {
                x0 = x0.min(p[0]);
                y0 = y0.min(p[1]);
                x1 = x1.max(p[0]);
                y1 = y1.max(p[1]);
            }
        }
        let pad = sh.stroke.map(|_| sh.stroke_width).unwrap_or(0.0) + 1.0;
        let (ix0, iy0, ix1, iy1) = (
            (x0 - pad).floor().max(0.0) as u32,
            (y0 - pad).floor().max(0.0) as u32,
            ((x1 + pad).ceil().min(width as f32)) as u32,
            ((y1 + pad).ceil().min(height as f32)) as u32,
        );
        if ix0 >= ix1 || iy0 >= iy1 {
            continue;
        }
        let half = sh.stroke_width / 2.0;
        for y in iy0..iy1 {
            for x in ix0..ix1 {
                let p = [x as f32 + 0.5, y as f32 + 0.5];
                let di = ((y * width + x) * 4) as usize;
                if let Some(c) = sh.fill {
                    if inside(&sh.polys, p, sh.evenodd) {
                        blend_px(&mut px[di..di + 4], c.r, c.g, c.b, c.a);
                    }
                }
                if let Some(c) = sh.stroke {
                    if stroke_inside(&sh.polys, &sh.closed_flags, p, half) {
                        blend_px(&mut px[di..di + 4], c.r, c.g, c.b, c.a);
                    }
                }
            }
        }
    }
    Some((width, height, px))
}

/// Rasterize at the document's intrinsic size.
pub fn rasterize_at_intrinsic(bytes: &[u8]) -> Option<(u32, u32, Vec<u8>)> {
    let (w, h) = intrinsic_size(bytes)?;
    rasterize(bytes, w, h)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn px_at(img: &[u8], w: u32, x: u32, y: u32) -> [u8; 4] {
        let i = ((y * w + x) * 4) as usize;
        [img[i], img[i + 1], img[i + 2], img[i + 3]]
    }

    #[test]
    fn rect_and_circle_fill() {
        let svg = br##"<svg width="40" height="20"><rect x="0" y="0" width="40" height="20" fill="#ff0000"/><circle cx="30" cy="10" r="6" fill="rgb(0,0,255)"/></svg>"##;
        let (w, h, img) = rasterize(svg, 40, 20).unwrap();
        assert_eq!((w, h), (40, 20));
        assert_eq!(px_at(&img, 40, 2, 10), [255, 0, 0, 255]);
        let c = px_at(&img, 40, 30, 10);
        assert!(c[2] > 200 && c[0] < 60, "circle paints blue over the rect: {c:?}");
    }

    #[test]
    fn path_and_viewbox_letterbox() {
        let svg = br##"<svg viewBox="0 0 100 100"><path d="M10 10 H90 V90 H10 Z" fill="green"/></svg>"##;
        let (w, h) = intrinsic_size(svg).unwrap();
        assert_eq!((w, h), (100, 100));
        let (_, _, img) = rasterize(svg, 20, 20).unwrap();
        assert_eq!(px_at(&img, 20, 10, 10)[1], 128, "green fill survives viewBox scaling");
        assert_eq!(px_at(&img, 20, 0, 0)[3], 0, "outside the viewBox stays transparent");
    }

    #[test]
    fn stroke_and_none_fill() {
        let svg = br##"<svg width="30" height="30"><rect x="5" y="5" width="20" height="20" fill="none" stroke="#00ff00" stroke-width="4"/></svg>"##;
        let (_, _, img) = rasterize(svg, 30, 30).unwrap();
        assert_eq!(px_at(&img, 30, 15, 15)[3], 0, "fill=none leaves the interior transparent");
        let e = px_at(&img, 30, 5, 15);
        assert!(e[1] > 200 && e[3] > 200, "stroke paints the edge: {e:?}");
    }

    #[test]
    fn ellipse_and_polygon() {
        let svg = br##"<svg width="40" height="40"><ellipse cx="20" cy="20" rx="14" ry="6" fill="red"/><polygon points="0,0 10,0 5,10" fill="blue"/></svg>"##;
        let (_, _, img) = rasterize(svg, 40, 40).unwrap();
        assert_eq!(px_at(&img, 40, 20, 20), [255, 0, 0, 255]);
        let t = px_at(&img, 40, 5, 3);
        assert!(t[2] > 200, "polygon paints: {t:?}");
    }

    #[test]
    fn rejects_non_svg() {
        assert!(rasterize(b"not xml at all", 10, 10).is_none());
        assert!(intrinsic_size(b"<html></html>").is_none());
    }
}
