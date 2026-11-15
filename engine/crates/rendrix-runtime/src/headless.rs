//! Headless driver: runs an application against an offscreen GPU target, optionally replaying a
//! scripted interaction. This exercises the same `Session` input path as the window driver, so it
//! doubles as the end-to-end test harness.

use crate::bundle::AppBundle;
use crate::session::{KeyInput, MemoryClipboard, Mods, Session};
use rendrix_gfx::{GpuContext, Renderer};
use serde_json::Value;
use std::time::{Duration, Instant};

pub struct Headless {
    pub session: Session,
    renderer: Renderer,
    pub size: (u32, u32),
    pub scale: f32,
    pub adapter: String,
}

pub struct Shot {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

impl Shot {
    pub fn save_png(&self, path: &std::path::Path) -> Result<(), String> {
        image::save_buffer(path, &self.rgba, self.width, self.height, image::ColorType::Rgba8).map_err(|e| e.to_string())
    }

    /// Save as a one-page PDF with the pixels as an uncompressed RGB image.
    ///
    /// Used by `rendrix render page.html -o out.pdf`. Deliberately dependency-free
    /// (raw RGB, no filters): a 800×600 page is ~1.4 MB. `/video` owns mp4; PDF and
    /// frame sequences are the only render outputs here.
    pub fn save_pdf(&self, path: &std::path::Path) -> Result<(), String> {
        // Minimal PDF 1.4: catalog → pages → page → RGB image XObject.
        // The content matrix flips vertically so the top pixel row stays on top.
        let rgb: Vec<u8> = self.rgba.chunks_exact(4).flat_map(|p| [p[0], p[1], p[2]]).collect();
        let content = format!("q {} 0 0 -{} 0 {} cm /Im0 Do Q", self.width, self.height, self.height);
        let mut pdf: Vec<u8> = b"%PDF-1.4\n".to_vec();
        let mut offsets: Vec<(u32, usize)> = Vec::new();
        let mut obj = |n: u32, body: &[u8]| {
            offsets.push((n, pdf.len()));
            pdf.extend_from_slice(format!("{n} 0 obj\n").as_bytes());
            pdf.extend_from_slice(body);
            pdf.extend_from_slice(b"\nendobj\n");
        };
        obj(1, b"<< /Type /Catalog /Pages 2 0 R >>");
        obj(2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
        let page = format!(
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 {} {}] /Resources << /XObject << /Im0 4 0 R >> >> /Contents 5 0 R >>",
            self.width, self.height
        );
        obj(3, page.as_bytes());
        let mut img = format!(
            "<< /Type /XObject /Subtype /Image /Width {} /Height {} /ColorSpace /DeviceRGB /BitsPerComponent 8 /Length {} >>\nstream\n",
            self.width,
            self.height,
            rgb.len()
        )
        .into_bytes();
        img.extend_from_slice(&rgb);
        img.extend_from_slice(b"\nendstream");
        obj(4, &img);
        let mut stream = format!("<< /Length {} >>\nstream\n", content.len()).into_bytes();
        stream.extend_from_slice(content.as_bytes());
        stream.extend_from_slice(b"\nendstream");
        obj(5, &stream);
        let xref = pdf.len();
        pdf.extend_from_slice(b"xref\n0 6\n0000000000 65535 f \n");
        for n in 1..=5u32 {
            let off = offsets.iter().find(|(i, _)| *i == n).map(|(_, o)| *o).unwrap_or(0);
            pdf.extend_from_slice(format!("{off:010} 00000 n \n").as_bytes());
        }
        pdf.extend_from_slice(format!("trailer\n<< /Size 6 /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF").as_bytes());
        std::fs::write(path, pdf).map_err(|e| e.to_string())
    }

    pub fn pixel(&self, x: u32, y: u32) -> [u8; 4] {
        let i = ((y * self.width + x) * 4) as usize;
        [self.rgba[i], self.rgba[i + 1], self.rgba[i + 2], self.rgba[i + 3]]
    }
}

impl Headless {
    /// `size` is in logical pixels; the target is `size * scale` physical pixels.
    pub fn new(bundle: &AppBundle, size: (u32, u32), scale: f32) -> Result<Headless, String> {
        let ctx = GpuContext::headless()?;
        let adapter = ctx.adapter_name();
        let renderer = Renderer::new(ctx.device, ctx.queue, wgpu::TextureFormat::Rgba8Unorm);
        let session = Session::new(bundle, Box::new(MemoryClipboard::default()), (size.0 as f32, size.1 as f32, scale))?;
        Ok(Headless { session, renderer, size, scale, adapter })
    }

    /// Run frames until the application is idle (no animation, no pending frame request) or `max` elapses.
    pub fn settle(&mut self, max: Duration) {
        let end = Instant::now() + max;
        loop {
            let info = self.session.frame();
            if !info.continuous && !info.repainted {
                let wake = info.next_wake_s.map(|w| w - self.session.now_s());
                match wake {
                    Some(w) if w > 0.0 && w < 0.05 && Instant::now() < end => std::thread::sleep(Duration::from_secs_f64(w.max(0.001))),
                    _ => break,
                }
            } else if info.continuous {
                std::thread::sleep(Duration::from_millis(4));
            }
            if Instant::now() >= end {
                break;
            }
        }
    }

    /// Advance real time by `ms`, running frames as a window would.
    pub fn wait(&mut self, ms: u64) {
        let end = Instant::now() + Duration::from_millis(ms);
        while Instant::now() < end {
            self.session.frame();
            std::thread::sleep(Duration::from_millis(4));
        }
        self.session.frame();
    }

    /// Render the current frame and read it back.
    pub fn screenshot(&mut self) -> Shot {
        self.session.frame();
        let (w, h) = ((self.size.0 as f32 * self.scale).round() as u32, (self.size.1 as f32 * self.scale).round() as u32);
        let engine = self.session.engine.borrow();
        for img in engine.take_image_uploads() {
            self.renderer.upload_image_as(img.id, &img.rgba, img.width, img.height);
        }
        // A3 canvas: create-or-refresh per-canvas GPU targets (partial re-upload).
        for up in rendrix_js::canvas_ops::take_canvas_uploads(&engine) {
            if up.rgba.is_empty() { self.renderer.remove_canvas(up.node); continue; }
            match up.dirty {
                Some(r) if r[2] * r[3] * 2 < up.width * up.height => {
                    self.renderer.update_canvas_region(up.node, &up.rgba, up.width, up.height, r)
                }
                _ => self.renderer.upload_canvas(up.node, &up.rgba, up.width, up.height),
            }
        }
        let mut text = engine.text_borrow();
        let rgba = self.renderer.render_to_rgba(engine.display_list(), &mut text, (w, h), self.scale);
        Shot { width: w, height: h, rgba }
    }

    /// Save `frames` screenshots to `dir` as `frame-0001.png`, …, advancing `frame_ms`
    /// of application time between frames (drives rAF/animations). The frame sequence is
    /// the handoff to the /video pipeline, which owns mp4 encoding.
    pub fn save_frames(&mut self, dir: &std::path::Path, frames: usize, frame_ms: u64) -> Result<Vec<std::path::PathBuf>, String> {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        let mut out = Vec::new();
        for i in 0..frames.max(1) {
            if i > 0 {
                self.wait(frame_ms);
            } else {
                self.session.frame();
            }
            let p = dir.join(format!("frame-{:04}.png", i + 1));
            self.screenshot().save_png(&p)?;
            println!("[frame] {}", p.display());
            out.push(p);
        }
        Ok(out)
    }

    /// Render the current display list into an offscreen target and wait for the GPU; returns the elapsed time.
    pub fn render_timed(&mut self) -> Duration {
        let (w, h) = ((self.size.0 as f32 * self.scale).round() as u32, (self.size.1 as f32 * self.scale).round() as u32);
        let t = Instant::now();
        let view = self.target_view(w, h);
        let engine = self.session.engine.borrow();
        for img in engine.take_image_uploads() {
            self.renderer.upload_image_as(img.id, &img.rgba, img.width, img.height);
        }
        // A3 canvas: create-or-refresh per-canvas GPU targets (partial re-upload).
        for up in rendrix_js::canvas_ops::take_canvas_uploads(&engine) {
            if up.rgba.is_empty() { self.renderer.remove_canvas(up.node); continue; }
            match up.dirty {
                Some(r) if r[2] * r[3] * 2 < up.width * up.height => {
                    self.renderer.update_canvas_region(up.node, &up.rgba, up.width, up.height, r)
                }
                _ => self.renderer.upload_canvas(up.node, &up.rgba, up.width, up.height),
            }
        }
        let mut text = engine.text_borrow();
        self.renderer.render(engine.display_list(), &mut text, &view, (w, h), self.scale);
        let _ = self.renderer.device().poll(wgpu::PollType::Wait);
        t.elapsed()
    }

    fn target_view(&mut self, w: u32, h: u32) -> wgpu::TextureView {
        let tex = self.renderer.device().create_texture(&wgpu::TextureDescriptor {
            label: Some("rendrix bench target"),
            size: wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            view_formats: &[],
        });
        tex.create_view(&Default::default())
    }

    /// (instances, batches) of the last GPU frame.
    pub fn render_stats(&self) -> (usize, usize) {
        (self.renderer.last_instances, self.renderer.last_batches)
    }

    pub fn click(&mut self, x: f32, y: f32) {
        self.session.pointer_move(x, y);
        self.session.pointer_down(x, y, 0);
        self.session.pointer_up(x, y, 0);
    }

    /// Inject file-dialog results without OS UI (`Some(paths)` picks, `None` cancels).
    pub fn set_dialog_mock(&mut self, result: Option<Option<Vec<String>>>) {
        self.session.js.host.borrow_mut().set_dialog_mock(result);
    }

    /// Simulate an OS file drop at viewport coordinates, through the same public
    /// `JsRuntime::dispatch` path the window driver uses for `DroppedFile`.
    pub fn drop_files(&mut self, x: f32, y: f32, paths: &[&str]) {
        let target = self.session.engine.borrow().hit_test(x, y);
        let files: Vec<Value> = paths.iter().map(|p| serde_json::json!({ "name": p.rsplit('/').next().unwrap_or(p), "path": p })).collect();
        self.session.js.dispatch("drop", target, &serde_json::json!({ "clientX": x, "clientY": y, "button": 0, "dataTransfer": { "files": files, "dropEffect": "copy" } }));
    }

    /// A second window over the same bundle: an independent `Session` (engine + JS context),
    /// exactly like `window.rs` builds for `window.open`.
    pub fn open_secondary(&self, bundle: &AppBundle) -> Result<Headless, String> {
        Headless::new(bundle, self.size, self.scale)
    }

    pub fn click_selector(&mut self, selector: &str) -> Result<(), String> {
        self.session.frame();
        let id = self.session.query(selector).ok_or_else(|| format!("no element matches '{selector}'"))?;
        let (x, y) = self.session.center_of(id).ok_or_else(|| format!("'{selector}' has no box"))?;
        self.click(x, y);
        Ok(())
    }

    pub fn type_text(&mut self, text: &str) {
        for ch in text.chars() {
            let s = ch.to_string();
            let code = if ch.is_ascii_alphabetic() { format!("Key{}", ch.to_ascii_uppercase()) } else { String::new() };
            let k = KeyInput { key: s.clone(), code, text: Some(s), repeat: false, mods: Mods::default() };
            self.session.key_down(k.clone());
            self.session.key_up(k);
        }
    }

    pub fn press(&mut self, key: &str, mods: Mods) {
        let k = KeyInput { key: key.to_string(), code: key.to_string(), text: None, repeat: false, mods };
        self.session.key_down(k.clone());
        self.session.key_up(k);
    }

    /// Evaluate a JavaScript expression and render its value as text.
    pub fn eval(&mut self, js: &str) -> Result<String, String> {
        let wrapped = format!("(()=>{{ const v = ({js}); return typeof v === 'string' ? v : JSON.stringify(v); }})()");
        match self.session.js.eval(&wrapped) {
            Ok(rendrix_js::Val::Str(s)) => Ok(s),
            Ok(other) => Ok(format!("{other:?}")),
            Err(e) => Err(e.to_string()),
        }
    }

    /// Replay a list of steps. Returns the number of steps run; the first failure is an error.
    pub fn run_steps(&mut self, steps: &[Value], out_dir: &std::path::Path) -> Result<usize, String> {
        for (i, step) in steps.iter().enumerate() {
            let kind = step["type"].as_str().unwrap_or("");
            let fail = |m: String| format!("step {} ({kind}): {m}", i + 1);
            match kind {
                "wait" => self.wait(step["ms"].as_u64().unwrap_or(50)),
                "settle" => self.settle(Duration::from_millis(step["ms"].as_u64().unwrap_or(2000))),
                "move" => {
                    let (x, y) = self.target(step).map_err(&fail)?;
                    self.session.pointer_move(x, y);
                }
                "click" => {
                    if let Some(sel) = step["selector"].as_str() {
                        self.click_selector(sel).map_err(&fail)?;
                    } else {
                        self.click(step["x"].as_f64().unwrap_or(0.0) as f32, step["y"].as_f64().unwrap_or(0.0) as f32);
                    }
                }
                "type" => self.type_text(step["text"].as_str().unwrap_or("")),
                "key" => {
                    let m = Mods { ctrl: step["ctrl"].as_bool().unwrap_or(false), shift: step["shift"].as_bool().unwrap_or(false), alt: step["alt"].as_bool().unwrap_or(false), meta: false };
                    self.press(step["key"].as_str().unwrap_or(""), m);
                }
                "wheel" => {
                    let (x, y) = self.target(step).unwrap_or((10.0, 10.0));
                    self.session.wheel(x, y, step["dx"].as_f64().unwrap_or(0.0) as f32, step["dy"].as_f64().unwrap_or(0.0) as f32);
                }
                "resize" => {
                    self.size = (step["width"].as_u64().unwrap_or(self.size.0 as u64) as u32, step["height"].as_u64().unwrap_or(self.size.1 as u64) as u32);
                    self.session.resize(self.size.0 as f32, self.size.1 as f32, self.scale);
                }
                "eval" => {
                    let v = self.eval(step["js"].as_str().unwrap_or("undefined")).map_err(&fail)?;
                    println!("[eval] {v}");
                }
                "expect" => {
                    self.session.frame();
                    let js = step["js"].as_str().unwrap_or("undefined");
                    let got = self.eval(js).map_err(&fail)?;
                    let want = step["equals"].as_str().map(str::to_string).unwrap_or_else(|| step["equals"].to_string());
                    if got != want {
                        return Err(fail(format!("`{js}` was {got}, expected {want}")));
                    }
                    println!("[expect] ok: {js} == {want}");
                }
                "screenshot" => {
                    let path = out_dir.join(step["path"].as_str().unwrap_or("screenshot.png"));
                    self.screenshot().save_png(&path).map_err(&fail)?;
                    println!("[screenshot] {}", path.display());
                }
                other => return Err(fail(format!("unknown step type '{other}'"))),
            }
        }
        Ok(steps.len())
    }

    fn target(&mut self, step: &Value) -> Result<(f32, f32), String> {
        if let Some(sel) = step["selector"].as_str() {
            self.session.frame();
            let id = self.session.query(sel).ok_or_else(|| format!("no element matches '{sel}'"))?;
            return self.session.center_of(id).ok_or_else(|| format!("'{sel}' has no box"));
        }
        Ok((step["x"].as_f64().unwrap_or(0.0) as f32, step["y"].as_f64().unwrap_or(0.0) as f32))
    }
}
