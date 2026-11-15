//! Rendrix for the browser.
//!
//! The same engine as the desktop runtime (CSS, DOM, layout, text, wgpu renderer, the Web API
//! prelude), compiled to WebAssembly. It draws to an `OffscreenCanvas` through WebGPU (WebGL2 where
//! WebGPU is missing) and runs the app's JavaScript on the browser's own engine.
//!
//! It runs in a Web Worker: the app's prelude replaces `window`/`document`/`fetch`/timers in its
//! realm, so it must not share one with the host page. The page owns the real `<canvas>` and the
//! OS events and forwards them (see `web/rendrix-web.js`); the worker calls [`WebApp::frame`] on
//! its own animation-frame loop and reports back what the page must do (cursor, soft keyboard, …).

#![cfg(target_arch = "wasm32")]

mod engine;

use engine::{console_log, do_fetch, random_f64, BrowserEngine};
use rendrix_gfx::{GpuContext, Renderer};
use rendrix_js::host::{FetchResult, HttpRequest};
use rendrix_runtime::session::{Clipboard, GamepadState, KeyInput, Mods};
use rendrix_runtime::{AppBundle, Session};
use serde_json::json;
use std::cell::RefCell;
use std::rc::Rc;
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::JsFuture;

struct WebClipboard {
    text: Rc<RefCell<String>>,
    /// Text the app copied since the page last asked: the page performs the real clipboard write.
    pending: Rc<RefCell<Option<String>>>,
}

impl Clipboard for WebClipboard {
    fn get(&mut self) -> String {
        self.text.borrow().clone()
    }
    fn set(&mut self, text: &str) {
        *self.text.borrow_mut() = text.to_string();
        *self.pending.borrow_mut() = Some(text.to_string());
    }
}

#[wasm_bindgen]
pub struct WebApp {
    session: Session,
    renderer: Renderer,
    surface: wgpu::Surface<'static>,
    config: wgpu::SurfaceConfiguration,
    canvas: web_sys::OffscreenCanvas,
    size: (u32, u32),
    scale: f32,
    frames: u64,
    clip_text: Rc<RefCell<String>>,
    clip_pending: Rc<RefCell<Option<String>>>,
    adapter: String,
}

fn install_panic_hook() {
    std::panic::set_hook(Box::new(|info| console_log("error", &format!("[rendrix] panic: {info}"))));
}

fn fetch_result(id: u32, status: u16, status_text: String, headers: Vec<(String, String)>, body: Vec<u8>, error: Option<String>) -> FetchResult {
    FetchResult { id, status, status_text, headers, body, error, releases_slot: true }
}

/// Browser `fetch` behind the host's HTTP hook (the host's native build spawns a thread instead).
fn http_backend(req: HttpRequest) {
    wasm_bindgen_futures::spawn_local(async move {
        let headers: js_sys::Array = req.headers.iter().flat_map(|(k, v)| [JsValue::from_str(k), JsValue::from_str(v)]).collect();
        let body: JsValue = req.body.as_ref().map(|b| js_sys::Uint8Array::from(&b[..]).into()).unwrap_or(JsValue::NULL);
        let result = match JsFuture::from(do_fetch(&req.url, &req.method, &headers, &body)).await {
            Ok(v) => {
                let get = |k: &str| js_sys::Reflect::get(&v, &JsValue::from_str(k)).unwrap_or(JsValue::UNDEFINED);
                let flat = js_sys::Array::from(&get("headers"));
                let hdrs: Vec<(String, String)> = (0..flat.length() / 2).map(|i| (flat.get(i * 2).as_string().unwrap_or_default(), flat.get(i * 2 + 1).as_string().unwrap_or_default())).collect();
                let bytes = js_sys::Uint8Array::new(&get("body")).to_vec();
                fetch_result(req.id, get("status").as_f64().unwrap_or(0.0) as u16, get("statusText").as_string().unwrap_or_default(), hdrs, bytes, None)
            }
            Err(e) => {
                let msg = js_sys::Reflect::get(&e, &JsValue::from_str("message")).ok().and_then(|m| m.as_string()).unwrap_or_else(|| "network error".to_string());
                fetch_result(req.id, 0, String::new(), vec![], vec![], Some(msg))
            }
        };
        let _ = req.reply.send(result);
    });
}

/// Boot an app on `canvas` (transferred from the page). `bundle_json` is a `rendrix-app/1` bundle
/// as produced by `rendrix build`. Sizes are CSS pixels; `dpr` is the device pixel ratio.
#[wasm_bindgen]
pub async fn start(canvas: web_sys::OffscreenCanvas, bundle_json: String, css_w: f32, css_h: f32, dpr: f32) -> Result<WebApp, JsValue> {
    install_panic_hook();
    let bundle = AppBundle::from_json(&bundle_json).map_err(|e| JsValue::from_str(&format!("invalid app bundle: {e}")))?;
    let size = (((css_w * dpr).round() as u32).max(1), ((css_h * dpr).round() as u32).max(1));
    canvas.set_width(size.0);
    canvas.set_height(size.1);

    // Probe for a *usable* WebGPU adapter first: the browser may expose `navigator.gpu`
    // without being able to create an adapter (headless SwiftShader), and a plain
    // `Instance::new` would then lock onto the WebGPU-only path with no WebGL2 fallback.
    let instance = wgpu::util::new_instance_with_webgpu_detection(&wgpu::InstanceDescriptor { backends: rendrix_gfx::context::default_backends(), ..Default::default() }).await;
    let surface = instance.create_surface(wgpu::SurfaceTarget::OffscreenCanvas(canvas.clone())).map_err(|e| JsValue::from_str(&format!("create surface: {e}")))?;
    let ctx = GpuContext::new_async(Some(&surface), Some(instance)).await.map_err(|e| JsValue::from_str(&e))?;
    let caps = surface.get_capabilities(&ctx.adapter);
    // Blend in sRGB space like CSS does: prefer a non-sRGB surface format.
    let format = caps.formats.iter().find(|f| !f.is_srgb()).copied().or_else(|| caps.formats.first().copied()).ok_or_else(|| JsValue::from_str("the canvas reports no surface formats"))?;
    let alpha_mode = caps.alpha_modes.iter().copied().find(|m| *m == wgpu::CompositeAlphaMode::Opaque).or_else(|| caps.alpha_modes.first().copied()).unwrap_or(wgpu::CompositeAlphaMode::Auto);
    let config = wgpu::SurfaceConfiguration { usage: wgpu::TextureUsages::RENDER_ATTACHMENT, format, width: size.0, height: size.1, present_mode: wgpu::PresentMode::AutoVsync, desired_maximum_frame_latency: 2, alpha_mode, view_formats: vec![] };
    surface.configure(&ctx.device, &config);
    let adapter = ctx.adapter_name();
    let renderer = Renderer::new(ctx.device, ctx.queue, format);

    let clip_text = Rc::new(RefCell::new(String::new()));
    let clip_pending = Rc::new(RefCell::new(None));
    let clipboard = Box::new(WebClipboard { text: clip_text.clone(), pending: clip_pending.clone() });
    let session = Session::new_with_engine(&bundle, clipboard, (css_w, css_h, dpr), |host| {
        {
            let mut h = host.borrow_mut();
            h.echo_console = false;
            h.rng = Some(Box::new(random_f64));
            h.http_backend = Some(Rc::new(http_backend));
        }
        Ok(Box::new(BrowserEngine::new(host)?) as Box<dyn rendrix_js::JsEngine>)
    })
    .map_err(|e| JsValue::from_str(&e))?;
    Ok(WebApp { session, renderer, surface, config, canvas, size, scale: dpr, frames: 0, clip_text, clip_pending, adapter })
}

fn cursor_css(c: rendrix_css::Cursor) -> &'static str {
    use rendrix_css::Cursor::*;
    match c {
        Pointer => "pointer",
        Text => "text",
        Move => "move",
        NotAllowed => "not-allowed",
        Grab => "grab",
        Grabbing => "grabbing",
        Crosshair => "crosshair",
        Wait => "wait",
        Help => "help",
        ColResize => "col-resize",
        RowResize => "row-resize",
        Auto | Default => "default",
    }
}

#[wasm_bindgen]
impl WebApp {
    /// Adapter and surface description, for diagnostics ("NVIDIA … (BrowserWebGpu)").
    pub fn adapter(&self) -> String {
        self.adapter.clone()
    }

    /// Run one frame: timers, rAF, style, layout, paint, present. Returns a JSON report telling the
    /// page what to do next (see the doc on the module).
    pub fn frame(&mut self) -> String {
        let info = self.session.frame();
        let console = self.session.js.host.borrow_mut().take_console();
        for (level, msg) in console {
            console_log(&level, &msg);
        }
        let mut presented = false;
        if info.repainted || self.frames == 0 {
            presented = self.present();
        }
        let now = self.session.now_s();
        let wake_ms = info.next_wake_s.map(|w| ((w - now) * 1000.0).max(0.0));
        let external: Vec<String> = std::mem::take(&mut self.session.js.host.borrow_mut().external_urls);
        let text_rects: Option<Vec<[f32; 4]>> = info.repainted.then(|| self.text_rects());
        json!({
            "repainted": presented,
            "continuous": info.continuous,
            "nextWakeMs": wake_ms,
            "textInput": info.text_input,
            "title": info.title,
            "caret": self.session.caret_rect(),
            "cursor": info.cursor.map(cursor_css),
            "quit": info.quit,
            "clipboard": self.clip_pending.borrow_mut().take(),
            "external": external,
            "textRects": text_rects,
        })
        .to_string()
    }

    fn text_rects(&self) -> Vec<[f32; 4]> {
        let e = self.session.engine.borrow();
        let mut out = Vec::new();
        for tag in ["input", "textarea"] {
            for id in e.doc.get_elements_by_tag_name(e.doc.root(), tag) {
                if e.doc.is_text_control(id) {
                    if let Some(r) = e.element_rect(id) {
                        if r[2] > 0.0 && r[3] > 0.0 {
                            out.push(r);
                        }
                    }
                }
            }
        }
        out.truncate(64);
        out
    }

    /// Paint the current display list to the canvas. Returns whether a frame was presented.
    fn present(&mut self) -> bool {
        let frame = match self.surface.get_current_texture() {
            Ok(f) => f,
            Err(wgpu::SurfaceError::Outdated | wgpu::SurfaceError::Lost) => {
                self.surface.configure(self.renderer.device(), &self.config);
                return false;
            }
            Err(_) => return false,
        };
        let view = frame.texture.create_view(&Default::default());
        {
            let engine = self.session.engine.borrow();
            for img in engine.take_image_uploads() {
                self.renderer.upload_image_as(img.id, &img.rgba, img.width, img.height);
            }
            for up in rendrix_js::canvas_ops::take_canvas_uploads(&engine) {
                if up.rgba.is_empty() {
                    self.renderer.remove_canvas(up.node);
                    continue;
                }
                match up.dirty {
                    Some(r) if r[2] * r[3] * 2 < up.width * up.height => self.renderer.update_canvas_region(up.node, &up.rgba, up.width, up.height, r),
                    _ => self.renderer.upload_canvas(up.node, &up.rgba, up.width, up.height),
                }
            }
            let mut text = engine.text_borrow();
            self.renderer.render(engine.display_list(), &mut text, &view, self.size, self.scale);
        }
        frame.present();
        self.frames += 1;
        true
    }

    /// The canvas was resized (CSS px) or moved to a display with another pixel ratio.
    pub fn resize(&mut self, css_w: f32, css_h: f32, dpr: f32) {
        let size = (((css_w * dpr).round() as u32).max(1), ((css_h * dpr).round() as u32).max(1));
        self.scale = dpr;
        if size != self.size {
            self.size = size;
            self.canvas.set_width(size.0);
            self.canvas.set_height(size.1);
            self.config.width = size.0;
            self.config.height = size.1;
            self.surface.configure(self.renderer.device(), &self.config);
        }
        self.session.resize(css_w, css_h, dpr);
    }

    // ---- pointer / touch / wheel -------------------------------------------------------------

    pub fn pointer_move(&mut self, x: f32, y: f32) {
        self.session.pointer_move(x, y);
    }
    pub fn pointer_down(&mut self, x: f32, y: f32, button: u32) {
        self.session.pointer_down(x, y, button);
    }
    pub fn pointer_up(&mut self, x: f32, y: f32, button: u32) {
        self.session.pointer_up(x, y, button);
    }
    pub fn wheel(&mut self, x: f32, y: f32, dx: f32, dy: f32) {
        self.session.wheel(x, y, dx, dy);
    }
    pub fn touch_start(&mut self, id: u32, x: f32, y: f32) {
        self.session.touch_start(id, x, y);
    }
    pub fn touch_move(&mut self, id: u32, x: f32, y: f32) {
        self.session.touch_move(id, x, y);
    }
    pub fn touch_end(&mut self, id: u32, x: f32, y: f32) {
        self.session.touch_end(id, x, y);
    }
    pub fn touch_cancel(&mut self, id: u32) {
        self.session.touch_cancel(id);
    }

    // ---- keyboard / text ---------------------------------------------------------------------

    /// A key event. Printable text arrives separately through [`WebApp::text_commit`] (the page's
    /// hidden text input owns IME and soft keyboards), so `text` is not part of this call.
    pub fn key_down(&mut self, key: String, code: String, repeat: bool, ctrl: bool, shift: bool, alt: bool, meta: bool) {
        self.session.key_down(KeyInput { key, code, text: None, repeat, mods: Mods { ctrl, shift, alt, meta } });
    }
    pub fn key_up(&mut self, key: String, code: String, ctrl: bool, shift: bool, alt: bool, meta: bool) {
        self.session.key_up(KeyInput { key, code, text: None, repeat: false, mods: Mods { ctrl, shift, alt, meta } });
    }
    pub fn text_commit(&mut self, text: String) {
        self.session.text_commit(&text);
    }
    /// The page saw a paste: make `text` the clipboard, then run the paste key action.
    pub fn paste(&mut self, text: String) {
        *self.clip_text.borrow_mut() = text;
        self.session.key_down(KeyInput { key: "v".into(), code: "KeyV".into(), text: None, repeat: false, mods: Mods { ctrl: true, ..Default::default() } });
        self.session.key_up(KeyInput { key: "v".into(), code: "KeyV".into(), text: None, repeat: false, mods: Mods { ctrl: true, ..Default::default() } });
    }

    // ---- environment -------------------------------------------------------------------------

    pub fn set_window_focused(&mut self, focused: bool) {
        self.session.set_window_focused(focused);
    }
    pub fn set_dark(&mut self, dark: bool) {
        self.session.engine.borrow_mut().set_dark(dark);
    }
    pub fn set_safe_area(&mut self, top: f32, right: f32, bottom: f32, left: f32) {
        self.session.set_safe_area(top, right, bottom, left);
    }
    pub fn set_keyboard_height(&mut self, height: f32) {
        self.session.set_keyboard_height(height);
    }
    pub fn set_max_touch_points(&mut self, n: u32) {
        self.session.set_max_touch_points(n);
    }
    pub fn set_gamepad(&mut self, index: usize, id: String, buttons: Vec<f32>, axes: Vec<f32>) {
        self.session.set_gamepad(index, Some(GamepadState { id, buttons, axes }));
    }
    pub fn clear_gamepad(&mut self, index: usize) {
        self.session.set_gamepad(index, None);
    }

    /// Evaluate a JS expression in the app (diagnostics and tests). Strings come back as-is,
    /// everything else JSON-encoded.
    pub fn eval(&mut self, expr: String) -> String {
        let wrapped = format!("(()=>{{ const v = ({expr}); return typeof v === 'string' ? v : JSON.stringify(v); }})()");
        match self.session.js.eval(&wrapped) {
            Ok(rendrix_js::Val::Str(s)) => s,
            Ok(other) => format!("{other:?}"),
            Err(e) => format!("error: {e}"),
        }
    }
}
