//! The native window driver: winit event loop → per-window `Session`, `Renderer` → wgpu surface.

use crate::bundle::AppBundle;
use crate::session::{KeyInput, Mods, Session, SystemClipboard};
use accesskit_winit::Adapter;
use rendrix_css::Cursor;
use rendrix_gfx::{GpuContext, Renderer};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use winit::application::ApplicationHandler;
use winit::dpi::{LogicalPosition, LogicalSize, PhysicalPosition};
use winit::event::{ElementState, Ime, KeyEvent, MouseButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy};
use winit::keyboard::{Key, ModifiersState, NamedKey, PhysicalKey};
use winit::window::{CursorIcon, Window, WindowAttributes, WindowId};

/// Everything the driver needs to present and interact with one native window. Each window owns
/// an independent `Session` (engine + JS context), so script state is isolated per window and an
/// idle window costs no work. Per-window application state is intentionally *not* shared: shared
/// documents/clipboard beyond the OS clipboard are follow-up work (see the A1 notes in
/// `docs/COMPATIBILITY.md`).
struct WindowState {
    window: Arc<Window>,
    surface: wgpu::Surface<'static>,
    config: wgpu::SurfaceConfiguration,
    renderer: Renderer,
    session: Session,
    cursor_pos: PhysicalPosition<f64>,
    /// Script-facing id from `window.open` (`None` for the primary window).
    script_id: Option<u32>,
    /// Screen-reader bridge. `Some` once the adapter is attached; updates are pushed only when
    /// assistive technology is active (`update_if_active`) and only on repainted frames.
    a11y: Option<Adapter>,
    /// A file drag is hovering this window (for `dragenter` vs `dragover` synthesis).
    hovered_file: bool,
    /// Soft keyboard / IME currently requested (mobile toggles it with the focused control).
    /// Desktop-only builds never read this (the IME stays enabled); it exists on mobile only.
    #[cfg(any(target_os = "android", target_os = "ios"))]
    ime_on: bool,
}

struct App {
    bundle: AppBundle,
    windows: HashMap<WindowId, WindowState>,
    mods: ModifiersState,
    error: Option<String>,
    start: Instant,
    frames: u64,
    /// Delivers AccessKit action requests to the main loop thread (AT callbacks may fire on any
    /// thread, but `Session` is main-thread only).
    a11y_proxy: Option<EventLoopProxy<accesskit_winit::Event>>,
    /// `RENDRIX_DUMP_FRAME=<file.png>` writes every presented frame (throttled) to that file.
    dump: Option<(std::path::PathBuf, Instant)>,
    /// `RENDRIX_TRACE_EVENTS=1` prints every window event (debugging input).
    trace: bool,
    /// D2 hot reload: mtime of the CLI sidecar file last consumed.
    hot_mtime: Option<std::time::SystemTime>,
    /// Desktop gamepads (`gilrs`); `None` when the OS backend is unavailable.
    #[cfg(not(any(target_os = "android", target_os = "ios")))]
    pads: Option<crate::gamepad::Pads>,
    /// Whether any gamepad was connected at the last poll (keeps the loop polling at ~60 Hz).
    pads_active: bool,
}

pub fn run(bundle: AppBundle) -> Result<(), String> {
    let event_loop = EventLoop::<accesskit_winit::Event>::with_user_event().build().map_err(|e| format!("event loop: {e}"))?;
    event_loop.set_control_flow(ControlFlow::Wait);
    let mut app = App { bundle, windows: HashMap::new(), mods: ModifiersState::empty(), error: None, start: Instant::now(), frames: 0, a11y_proxy: None, trace: std::env::var_os("RENDRIX_TRACE_EVENTS").is_some(), dump: std::env::var_os("RENDRIX_DUMP_FRAME").map(|p| (std::path::PathBuf::from(p), Instant::now() - Duration::from_secs(1))), hot_mtime: None, #[cfg(not(any(target_os = "android", target_os = "ios")))] pads: crate::gamepad::Pads::new(), pads_active: false };
    app.a11y_proxy = Some(event_loop.create_proxy());
    event_loop.run_app(&mut app).map_err(|e| format!("event loop: {e}"))?;
    match app.error.take() {
        Some(e) => Err(e),
        None => Ok(()),
    }
}

fn mods_of(m: ModifiersState) -> Mods {
    Mods { ctrl: m.control_key(), shift: m.shift_key(), alt: m.alt_key(), meta: m.super_key() }
}

fn dom_key(key: &Key) -> String {
    match key {
        Key::Character(s) => s.to_string(),
        Key::Named(n) => match n {
            NamedKey::Space => " ".into(),
            NamedKey::Control => "Control".into(),
            NamedKey::Super => "Meta".into(),
            other => format!("{other:?}"),
        },
        Key::Dead(_) => "Dead".into(),
        Key::Unidentified(_) => "Unidentified".into(),
    }
}

fn translate_key(ev: &KeyEvent, mods: ModifiersState) -> KeyInput {
    let code = match ev.physical_key {
        PhysicalKey::Code(c) => format!("{c:?}"),
        PhysicalKey::Unidentified(_) => "Unidentified".into(),
    };
    KeyInput { key: dom_key(&ev.logical_key), code, text: ev.text.as_ref().map(|t| t.to_string()), repeat: ev.repeat, mods: mods_of(mods) }
}

fn cursor_icon(c: Cursor) -> CursorIcon {
    match c {
        Cursor::Pointer => CursorIcon::Pointer,
        Cursor::Text => CursorIcon::Text,
        Cursor::Move => CursorIcon::Move,
        Cursor::NotAllowed => CursorIcon::NotAllowed,
        Cursor::Grab => CursorIcon::Grab,
        Cursor::Grabbing => CursorIcon::Grabbing,
        Cursor::Crosshair => CursorIcon::Crosshair,
        Cursor::Wait => CursorIcon::Wait,
        Cursor::Help => CursorIcon::Help,
        Cursor::ColResize => CursorIcon::ColResize,
        Cursor::RowResize => CursorIcon::RowResize,
        Cursor::Auto | Cursor::Default => CursorIcon::Default,
    }
}

/// `width`/`height` feature parsing for `window.open(url, target, features)` (`"width=800,height=600"`).
fn parse_window_features(features: &str, fallback: (u32, u32)) -> (u32, u32) {
    let mut w = fallback.0;
    let mut h = fallback.1;
    for part in features.split([',', ';']) {
        let mut kv = part.splitn(2, '=');
        match (kv.next().map(str::trim), kv.next().map(str::trim)) {
            (Some("width"), Some(v)) => {
                if let Ok(n) = v.parse::<u32>() {
                    w = n.clamp(120, 7680);
                }
            }
            (Some("height"), Some(v)) => {
                if let Ok(n) = v.parse::<u32>() {
                    h = n.clamp(80, 4320);
                }
            }
            _ => {}
        }
    }
    (w, h)
}

fn file_name_of(path: &std::path::Path) -> String {
    path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| path.to_string_lossy().into_owned())
}

/// Push the OS window insets into CSS `env(safe-area-inset-*)`.
///
/// winit 0.30 exposes no safe-area-insets API on any backend: on iOS `inner_size` already *is*
/// the safe area (so zero padding is correct there) and everywhere else there is nothing to
/// query. Native mobile shells that do know the insets (edge-to-edge Android, notched iOS)
/// feed them through [`Session::set_safe_area`] directly; this stays the single call site so a
/// future backend only changes this function. Re-run on resize because rotation can change the
/// insets; `set_safe_area` no-ops when the values are unchanged, so the steady state is free.
fn refresh_safe_area(state: &mut WindowState) {
    state.session.set_safe_area(0.0, 0.0, 0.0, 0.0);
}

impl App {
    /// Build one native window and boot a fresh `Session` (full app instance) in it.
    fn create_window_state(&mut self, event_loop: &ActiveEventLoop, width: u32, height: u32, title: &str, script_id: Option<u32>) -> Result<WindowId, String> {
        // AccessKit requires the adapter to exist before the window is shown for the first time.
        let attrs = WindowAttributes::default().with_visible(false).with_title(title).with_inner_size(LogicalSize::new(width as f64, height as f64));
        let window = Arc::new(event_loop.create_window(attrs).map_err(|e| format!("create window: {e}"))?);
        let id = window.id();
        let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor { backends: wgpu::Backends::PRIMARY, ..Default::default() });
        let surface = instance.create_surface(window.clone()).map_err(|e| format!("create surface: {e}"))?;
        let ctx = GpuContext::new(Some(&surface), Some(instance))?;
        let caps = surface.get_capabilities(&ctx.adapter);
        // Blend in sRGB space like CSS does: use a non-sRGB surface format.
        let format = caps.formats.iter().find(|f| !f.is_srgb()).copied().unwrap_or(caps.formats[0]);
        let size = window.inner_size();
        let alpha_mode = caps.alpha_modes.iter().copied().find(|m| *m == wgpu::CompositeAlphaMode::Opaque).unwrap_or(caps.alpha_modes[0]);
        let config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format,
            width: size.width.max(1),
            height: size.height.max(1),
            present_mode: wgpu::PresentMode::AutoVsync,
            desired_maximum_frame_latency: 2,
            alpha_mode,
            view_formats: vec![],
        };
        surface.configure(&ctx.device, &config);
        let renderer = Renderer::new(ctx.device, ctx.queue, format);
        let scale = window.scale_factor();
        let (lw, lh) = (size.width as f64 / scale, size.height as f64 / scale);
        if script_id.is_none() {
            eprintln!("[rendrix] adapter: {}  surface: {:?}  scale: {scale}", {
                let i = ctx.adapter.get_info();
                format!("{} ({:?})", i.name, i.backend)
            }, format);
        }
        let session = Session::new(&self.bundle, Box::new(SystemClipboard::new()), (lw as f32, lh as f32, scale as f32))?;
        window.set_ime_allowed(true);
        // Attach the screen-reader bridge before first show; a missing proxy (tests) skips it.
        let a11y = self.a11y_proxy.clone().map(|proxy| Adapter::with_event_loop_proxy(event_loop, &window, proxy));
        window.set_visible(true);
        window.request_redraw();
        self.windows.insert(id, WindowState { window, surface, config, renderer, session, cursor_pos: PhysicalPosition::new(0.0, 0.0), script_id, hovered_file: false, #[cfg(any(target_os = "android", target_os = "ios"))] ime_on: true, a11y });
        refresh_safe_area(self.windows.get_mut(&id).expect("just inserted"));
        Ok(id)
    }

    /// Secondary windows requested by script (`window.open`). Runs once per event-loop turn; each
    /// new window boots its own app instance, so this never blocks the frame loop beyond one boot.
    fn drain_window_requests(&mut self, event_loop: &ActiveEventLoop) {
        let mut opens = Vec::new();
        let mut closes = Vec::new();
        for state in self.windows.values() {
            let mut host = state.session.js.host.borrow_mut();
            opens.extend(std::mem::take(&mut host.window_open_requests).into_iter().map(|r| (r, state.window.inner_size())));
            closes.extend(std::mem::take(&mut host.window_close_requests));
        }
        for (req, _from_size) in opens {
            // New windows start at the bundle size unless `features` says otherwise.
            let (w, h) = parse_window_features(&req.features, (self.bundle.width, self.bundle.height));
            let title = if req.url.is_empty() { self.bundle.window_title() } else { format!("{} — {}", self.bundle.window_title(), req.url) };
            if let Err(e) = self.create_window_state(event_loop, w, h, &title, Some(req.id)) {
                eprintln!("[rendrix] window.open failed: {e}");
            }
        }
        if !closes.is_empty() {
            let ids: Vec<WindowId> = self.windows.iter().filter(|(_, s)| s.script_id.is_some_and(|id| closes.contains(&id))).map(|(wid, _)| *wid).collect();
            for wid in ids {
                self.windows.remove(&wid);
            }
            if self.windows.is_empty() {
                event_loop.exit();
            }
        }
    }

    fn logical(&self, id: WindowId, p: PhysicalPosition<f64>) -> (f32, f32) {
        let s = self.windows.get(&id).map(|g| g.window.scale_factor()).unwrap_or(1.0);
        ((p.x / s) as f32, (p.y / s) as f32)
    }

    /// Synthesize a drag-and-drop DOM event from an OS file event. Routed through the public
    /// `JsRuntime::dispatch` entry point, so no session-pump changes are needed.
    fn dispatch_files(&mut self, id: WindowId, kind: &str) {
        let Some(state) = self.windows.get_mut(&id) else { return };
        let (x, y) = {
            let s = state.window.scale_factor();
            ((state.cursor_pos.x / s) as f32, (state.cursor_pos.y / s) as f32)
        };
        let target = state.session.engine.borrow().hit_test(x, y);
        let init = serde_json::json!({ "clientX": x, "clientY": y, "button": 0, "dataTransfer": { "files": [], "dropEffect": "copy" } });
        state.session.js.dispatch(kind, target, &init);
    }

    fn dispatch_drop(&mut self, id: WindowId, paths: Vec<std::path::PathBuf>) {
        let Some(state) = self.windows.get_mut(&id) else { return };
        state.hovered_file = false;
        let (x, y) = {
            let s = state.window.scale_factor();
            ((state.cursor_pos.x / s) as f32, (state.cursor_pos.y / s) as f32)
        };
        let target = state.session.engine.borrow().hit_test(x, y);
        let files: Vec<serde_json::Value> = paths.iter().map(|p| serde_json::json!({ "name": file_name_of(p), "path": p.to_string_lossy() })).collect();
        let init = serde_json::json!({ "clientX": x, "clientY": y, "button": 0, "dataTransfer": { "files": files, "dropEffect": "copy" } });
        state.session.js.dispatch("drop", target, &init);
        state.window.request_redraw();
    }

    fn redraw(&mut self, event_loop: &ActiveEventLoop, id: WindowId) {
        let Some(state) = self.windows.get_mut(&id) else { return };
        let info = state.session.frame();
        if info.quit {
            event_loop.exit();
            return;
        }
        if let Some(t) = &info.title {
            state.window.set_title(t);
        }
        if let Some((w, h)) = info.resize {
            let _ = state.window.request_inner_size(LogicalSize::new(w as f64, h as f64));
        }
        if let Some(c) = info.cursor {
            state.window.set_cursor(cursor_icon(c));
        }
        // Mobile: show the soft keyboard exactly while a text control has focus. Desktop keeps the
        // IME enabled permanently (composition must work before the first keystroke).
        #[cfg(any(target_os = "android", target_os = "ios"))]
        if info.text_input != state.ime_on {
            state.ime_on = info.text_input;
            state.window.set_ime_allowed(info.text_input);
        }
        if let Some(r) = state.session.caret_rect() {
            state.window.set_ime_cursor_area(LogicalPosition::new(r[0] as f64, r[1] as f64), LogicalSize::new(r[2] as f64, r[3] as f64));
        }
        let size = state.window.inner_size();
        if size.width == 0 || size.height == 0 {
            return;
        }
        if info.repainted || self.frames == 0 {
            let frame = match state.surface.get_current_texture() {
                Ok(f) => f,
                Err(wgpu::SurfaceError::Outdated | wgpu::SurfaceError::Lost) => {
                    state.surface.configure(state.renderer.device(), &state.config);
                    state.window.request_redraw();
                    return;
                }
                Err(wgpu::SurfaceError::Timeout) => return,
                Err(e) => {
                    self.error = Some(format!("surface: {e}"));
                    event_loop.exit();
                    return;
                }
            };
            let view = frame.texture.create_view(&Default::default());
            {
                let engine = state.session.engine.borrow();
                for img in engine.take_image_uploads() {
                    state.renderer.upload_image_as(img.id, &img.rgba, img.width, img.height);
                }
                // A3 canvas: create-or-refresh per-canvas GPU targets (partial re-upload).
                for up in rendrix_js::canvas_ops::take_canvas_uploads(&engine) {
                    if up.rgba.is_empty() { state.renderer.remove_canvas(up.node); continue; }
                    match up.dirty {
                        Some(r) if r[2] * r[3] * 2 < up.width * up.height => {
                            state.renderer.update_canvas_region(up.node, &up.rgba, up.width, up.height, r)
                        }
                        _ => state.renderer.upload_canvas(up.node, &up.rgba, up.width, up.height),
                    }
                }
                let mut text = engine.text_borrow();
                state.renderer.render(engine.display_list(), &mut text, &view, (size.width, size.height), state.window.scale_factor() as f32);
            }
            state.window.pre_present_notify();
            frame.present();
            self.frames += 1;
            if let Some((path, last)) = self.dump.clone() {
                if last.elapsed() > Duration::from_millis(150) {
                    if let Some((_, last_mut)) = self.dump.as_mut() {
                        *last_mut = Instant::now();
                    }
                    let engine = state.session.engine.borrow();
                    let mut text = engine.text_borrow();
                    let scale = state.window.scale_factor() as f32;
                    let rgba = state.renderer.render_to_rgba(engine.display_list(), &mut text, (size.width, size.height), scale);
                    let _ = image::save_buffer(&path, &rgba, size.width, size.height, image::ColorType::Rgba8);
                }
            }
        }
        // Fresh display list → push the accessibility tree, but only while AT is listening.
        if info.repainted {
            if let Some(adapter) = state.a11y.as_mut() {
                let update = rendrix_js::host::a11y::build_tree(&state.session.js.host.borrow());
                adapter.update_if_active(|| update);
            }
        }
        if info.continuous {
            state.window.request_redraw();
        } else if let Some(w) = info.next_wake_s {
            let now = state.session.now_s();
            event_loop.set_control_flow(ControlFlow::WaitUntil(Instant::now() + Duration::from_secs_f64((w - now).max(0.0))));
        } else {
            event_loop.set_control_flow(ControlFlow::Wait);
        }
    }
}

impl ApplicationHandler<accesskit_winit::Event> for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if !self.windows.is_empty() {
            return;
        }
        let (w, h) = (self.bundle.width, self.bundle.height);
        let title = self.bundle.window_title();
        if let Err(e) = self.create_window_state(event_loop, w, h, &title, None) {
            self.error = Some(e);
            event_loop.exit();
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        self.drain_window_requests(event_loop);
        // D2 hot reload: consume the CLI sidecar (mtime-polled, never blocks the loop).
        if let Some(path) = std::env::var_os("RENDRIX_HOT_FILE") {
            let mtime = std::fs::metadata(&path).and_then(|m| m.modified()).ok();
            if mtime != self.hot_mtime {
                self.hot_mtime = mtime;
                if let Ok(text) = std::fs::read_to_string(&path) {
                    if let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) {
                        let modules: std::collections::BTreeMap<String, String> = v["modules"].as_object().map(|o| o.iter().map(|(k, v)| (k.clone(), v.as_str().unwrap_or("").to_string())).collect()).unwrap_or_default();
                        for state in self.windows.values_mut() {
                            match crate::hot_reload::apply_module_update(&mut state.session, modules.clone()) {
                                Ok(ran) => eprintln!("[hot] applied: {}", ran.join(", ")),
                                Err(e) => eprintln!("[hot] rejected: {e}"),
                            }
                            state.window.request_redraw();
                        }
                    }
                }
            }
        }
        // Gamepads have no OS wake-up here: poll them each loop turn and keep the loop ticking while one is connected.
        #[cfg(not(any(target_os = "android", target_os = "ios")))]
        if let Some(pads) = self.pads.as_mut() {
            let snapshot = pads.poll();
            self.pads_active = snapshot.iter().any(|(_, s)| s.is_some());
            for state in self.windows.values_mut() {
                for (i, s) in &snapshot {
                    state.session.set_gamepad(*i, s.clone());
                }
            }
        }
        let mut earliest: Option<f64> = None;
        let mut need_wait = false;
        for state in self.windows.values_mut() {
            let next = state.session.pump();
            let tick = state.session.js.tick_info();
            if state.session.engine.borrow().needs_update() || tick.wants_frame {
                state.window.request_redraw();
            } else if tick.pending_io {
                need_wait = true;
            } else if let Some(t) = next {
                earliest = Some(earliest.map_or(t, |e: f64| e.min(t)));
            }
        }
        if need_wait || self.pads_active {
            event_loop.set_control_flow(ControlFlow::WaitUntil(Instant::now() + Duration::from_millis(16)));
        } else if let Some(t) = earliest {
            // `pump` returns runtime-clock seconds; convert through any one session.
            let now = self.windows.values().next().map(|s| s.session.now_s()).unwrap_or(0.0);
            event_loop.set_control_flow(ControlFlow::WaitUntil(Instant::now() + Duration::from_secs_f64((t - now).max(0.0))));
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, id: WindowId, event: WindowEvent) {
        if self.trace && !matches!(event, WindowEvent::RedrawRequested) {
            eprintln!("[event] {event:?}");
        }
        if !self.windows.contains_key(&id) {
            return;
        }
        // AT adapters inspect every event before the application does.
        if let Some(state) = self.windows.get_mut(&id) {
            if let Some(a) = state.a11y.as_mut() {
                a.process_event(&state.window, &event);
            }
        }
        match event {
            WindowEvent::CloseRequested => {
                self.windows.remove(&id);
                if self.windows.is_empty() {
                    event_loop.exit();
                }
                return;
            }
            WindowEvent::RedrawRequested => {
                self.redraw(event_loop, id);
                return;
            }
            WindowEvent::Resized(size) => {
                if let Some(state) = self.windows.get_mut(&id) {
                    if size.width > 0 && size.height > 0 {
                        state.config.width = size.width;
                        state.config.height = size.height;
                        state.surface.configure(state.renderer.device(), &state.config);
                        let s = state.window.scale_factor();
                        state.session.resize((size.width as f64 / s) as f32, (size.height as f64 / s) as f32, s as f32);
                        refresh_safe_area(state);
                    }
                }
            }
            WindowEvent::ScaleFactorChanged { scale_factor, .. } => {
                if let Some(state) = self.windows.get_mut(&id) {
                    let size = state.window.inner_size();
                    state.session.resize((size.width as f64 / scale_factor) as f32, (size.height as f64 / scale_factor) as f32, scale_factor as f32);
                    refresh_safe_area(state);
                }
            }
            WindowEvent::ModifiersChanged(m) => {
                self.mods = m.state();
                if let Some(s) = self.windows.get_mut(&id) {
                    s.session.set_mods(mods_of(self.mods));
                }
            }
            WindowEvent::Focused(f) => {
                if let Some(s) = self.windows.get_mut(&id) {
                    s.session.set_window_focused(f);
                }
            }
            WindowEvent::CursorMoved { position, .. } => {
                let (x, y) = self.logical(id, position);
                if let Some(s) = self.windows.get_mut(&id) {
                    s.cursor_pos = position;
                    s.session.pointer_move(x, y);
                }
            }
            WindowEvent::CursorLeft { .. } => {
                if let Some(s) = self.windows.get_mut(&id) {
                    s.session.pointer_move(-1.0, -1.0);
                }
            }
            WindowEvent::MouseInput { state, button, .. } => {
                let b = match button {
                    MouseButton::Left => 0,
                    MouseButton::Middle => 1,
                    MouseButton::Right => 2,
                    MouseButton::Back => 3,
                    MouseButton::Forward => 4,
                    MouseButton::Other(n) => n as u32,
                };
                let (x, y) = self.windows.get(&id).map(|s| {
                    let f = s.window.scale_factor();
                    ((s.cursor_pos.x / f) as f32, (s.cursor_pos.y / f) as f32)
                }).unwrap_or((0.0, 0.0));
                if let Some(s) = self.windows.get_mut(&id) {
                    match state {
                        ElementState::Pressed => s.session.pointer_down(x, y, b),
                        ElementState::Released => s.session.pointer_up(x, y, b),
                    }
                }
            }
            WindowEvent::MouseWheel { delta, .. } => {
                let (pos, scale) = self.windows.get(&id).map(|s| (s.cursor_pos, s.window.scale_factor())).unwrap_or((PhysicalPosition::new(0.0, 0.0), 1.0));
                let (x, y) = ((pos.x / scale) as f32, (pos.y / scale) as f32);
                // winit: positive y scrolls content down (wheel up); the DOM's deltaY is the opposite.
                let (dx, dy) = match delta {
                    MouseScrollDelta::LineDelta(dx, dy) => (-dx * 48.0, -dy * 48.0),
                    MouseScrollDelta::PixelDelta(p) => ((-p.x / scale) as f32, ((-p.y / scale) as f32)),
                };
                if let Some(s) = self.windows.get_mut(&id) {
                    s.session.wheel(x, y, dx, dy);
                }
            }
            WindowEvent::Touch(touch) => {
                let s = self.windows.get(&id).map(|g| g.window.scale_factor()).unwrap_or(1.0);
                let (x, y) = ((touch.location.x / s) as f32, (touch.location.y / s) as f32);
                // winit touch ids are 64-bit; DOM touch identifiers are small ints.
                let tid = (touch.id & 0xffff) as u32;
                if let Some(st) = self.windows.get_mut(&id) {
                    if st.session.js.host.borrow().max_touch_points == 0 {
                        st.session.set_max_touch_points(5);
                    }
                    match touch.phase {
                        winit::event::TouchPhase::Started => st.session.touch_start(tid, x, y),
                        winit::event::TouchPhase::Moved => st.session.touch_move(tid, x, y),
                        winit::event::TouchPhase::Ended => st.session.touch_end(tid, x, y),
                        winit::event::TouchPhase::Cancelled => st.session.touch_cancel(tid),
                    }
                }
            }
            WindowEvent::KeyboardInput { event, .. } => {
                let k = translate_key(&event, self.mods);
                if let Some(s) = self.windows.get_mut(&id) {
                    match event.state {
                        ElementState::Pressed => s.session.key_down(k),
                        ElementState::Released => s.session.key_up(k),
                    }
                }
            }
            WindowEvent::Ime(Ime::Commit(text)) => {
                if let Some(s) = self.windows.get_mut(&id) {
                    s.session.text_commit(&text);
                }
            }
            WindowEvent::HoveredFile(path) => {
                let first = !self.windows.get(&id).is_some_and(|s| s.hovered_file);
                if let Some(s) = self.windows.get_mut(&id) {
                    s.hovered_file = true;
                }
                self.dispatch_files(id, if first { "dragenter" } else { "dragover" });
                let _ = path;
            }
            WindowEvent::HoveredFileCancelled => {
                if let Some(s) = self.windows.get_mut(&id) {
                    s.hovered_file = false;
                }
                self.dispatch_files(id, "dragleave");
            }
            WindowEvent::DroppedFile(path) => {
                self.dispatch_drop(id, vec![path]);
            }
            _ => return,
        }
        if let Some(w) = self.windows.get(&id) {
            w.window.request_redraw();
        }
    }

    fn user_event(&mut self, _event_loop: &ActiveEventLoop, event: accesskit_winit::Event) {
        match event.window_event {
            accesskit_winit::WindowEvent::ActionRequested(req) => self.a11y_action(event.window_id, req),
            accesskit_winit::WindowEvent::InitialTreeRequested => {
                // A platform adapter wants the tree now instead of on the next repaint.
                if let Some(state) = self.windows.get_mut(&event.window_id) {
                    if let Some(a) = state.a11y.as_mut() {
                        let update = rendrix_js::host::a11y::build_tree(&state.session.js.host.borrow());
                        a.update_if_active(|| update);
                    }
                }
            }
            accesskit_winit::WindowEvent::AccessibilityDeactivated => {}
        }
    }

    fn exiting(&mut self, _event_loop: &ActiveEventLoop) {
        let _ = self.start;
    }
}

impl App {
    /// Route a screen-reader action into the same `Session` entry points real input uses.
    /// Unmapped actions are ignored (the tree simply does not advertise them in v1, except
    /// where noted in `docs/COMPATIBILITY.md`).
    fn a11y_action(&mut self, window_id: WindowId, req: accesskit::ActionRequest) {
        use accesskit::{Action, ActionData};
        // Window root carries no actions.
        if req.target_node.0 == 0 {
            return;
        }
        let dom = req.target_node.0.saturating_sub(1) as u32;
        let Some(state) = self.windows.get_mut(&window_id) else { return };
        match req.action {
            Action::Click => {
                let center = state.session.engine.borrow().element_rect(dom).map(|r| (r[0] + r[2] / 2.0, r[1] + r[3] / 2.0));
                if let Some((x, y)) = center {
                    state.session.pointer_down(x, y, 0);
                    state.session.pointer_up(x, y, 0);
                }
            }
            Action::Focus => {
                state.session.engine.borrow_mut().doc.focus_modality_keyboard = true;
                state.session.js.set_focus(Some(dom));
            }
            Action::Blur => state.session.js.set_focus(None),
            Action::SetValue => {
                if let Some(ActionData::Value(v)) = req.data {
                    let v = v.to_string();
                    state.session.engine.borrow_mut().doc.set_form_value(dom, &v);
                    state.session.engine.borrow_mut().reset_caret();
                    state.session.js.fire_input(dom, "insertReplacementText", Some(&v));
                }
            }
            Action::ReplaceSelectedText => {
                if let Some(ActionData::Value(v)) = req.data {
                    let v = v.to_string();
                    let (cur, (a, b)) = {
                        let e = state.session.engine.borrow();
                        let cur = e.doc.value(dom);
                        let sel = e.doc.element(dom).map(|el| (el.form.selection_start.min(el.form.selection_end), el.form.selection_start.max(el.form.selection_end))).unwrap_or((cur.len(), cur.len()));
                        (cur, sel)
                    };
                    let mut next = cur;
                    next.replace_range(a.min(next.len())..b.min(next.len()), &v);
                    state.session.engine.borrow_mut().doc.set_form_value(dom, &next);
                    state.session.engine.borrow_mut().reset_caret();
                    state.session.js.fire_input(dom, "insertReplacementText", Some(&v));
                }
            }
            _ => {
                if self.trace {
                    eprintln!("[a11y] unhandled action {:?}", req.action);
                }
            }
        }
        state.window.request_redraw();
    }
}
