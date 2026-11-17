//! A running application: engine + JavaScript runtime + the input state machine.
//!
//! `Session` is platform-independent. The window driver and the headless driver both feed it
//! pointer, wheel, touch, keyboard and text events and call [`Session::frame`]; it turns them into DOM
//! events, default actions (focus, click activation, caret placement, text editing, Tab traversal)
//! and engine updates.

use crate::bundle::AppBundle;
use rendrix_core::{EditOp, Engine};
use rendrix_css::Cursor;
use rendrix_dom::{NodeId, NodeKind};
use rendrix_js::{JsRuntime, WindowCommand};
use serde_json::{json, Value};
use std::cell::RefCell;
use std::rc::Rc;
use web_time::Instant;

// Time the JS that builds the initial DOM (document scripts / entry module), separately from the
// font-DB load and the first frame. Thread-local so parallel bench/test threads stay independent.
thread_local! {
    static SCRIPT_RUN_NANOS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// Nanoseconds spent running document scripts (JS DOM construction) in the most recent
/// `Session::new*` call on this thread.
pub fn last_script_run_nanos() -> u64 {
    SCRIPT_RUN_NANOS.with(|c| c.get())
}

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Mods {
    pub ctrl: bool,
    pub shift: bool,
    pub alt: bool,
    pub meta: bool,
}

/// A keyboard event in DOM terms.
#[derive(Debug, Clone, Default)]
pub struct KeyInput {
    /// `KeyboardEvent.key` (`"a"`, `"Enter"`, `"ArrowLeft"`, …).
    pub key: String,
    /// `KeyboardEvent.code` (`"KeyA"`, `"Enter"`, …).
    pub code: String,
    /// Text the key produced, if any.
    pub text: Option<String>,
    pub repeat: bool,
    pub mods: Mods,
}

pub trait Clipboard {
    fn get(&mut self) -> String;
    fn set(&mut self, text: &str);
}

#[derive(Default)]
pub struct MemoryClipboard(pub String);
impl MemoryClipboard {
    pub fn new() -> Self {
        MemoryClipboard(String::new())
    }
}
/// Android/iOS have no system-clipboard backend in this build: use an in-process clipboard.
#[cfg(any(target_os = "android", target_os = "ios"))]
pub type SystemClipboard = MemoryClipboard;
impl Clipboard for MemoryClipboard {
    fn get(&mut self) -> String {
        self.0.clone()
    }
    fn set(&mut self, text: &str) {
        self.0 = text.to_string();
    }
}

#[cfg(not(any(target_os = "android", target_os = "ios", target_arch = "wasm32")))]
pub struct SystemClipboard(Option<arboard::Clipboard>);
#[cfg(not(any(target_os = "android", target_os = "ios", target_arch = "wasm32")))]
impl SystemClipboard {
    pub fn new() -> Self {
        SystemClipboard(arboard::Clipboard::new().ok())
    }
}
#[cfg(not(any(target_os = "android", target_os = "ios", target_arch = "wasm32")))]
impl Default for SystemClipboard {
    fn default() -> Self {
        Self::new()
    }
}
#[cfg(not(any(target_os = "android", target_os = "ios", target_arch = "wasm32")))]
impl Clipboard for SystemClipboard {
    fn get(&mut self) -> String {
        self.0.as_mut().and_then(|c| c.get_text().ok()).unwrap_or_default()
    }
    fn set(&mut self, text: &str) {
        if let Some(c) = self.0.as_mut() {
            let _ = c.set_text(text.to_string());
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct FrameInfo {
    /// The engine produced a new display list.
    pub repainted: bool,
    /// Another frame is needed right away (animations, rAF).
    pub continuous: bool,
    /// Runtime-clock seconds at which something time-driven needs attention.
    pub next_wake_s: Option<f64>,
    pub cursor: Option<Cursor>,
    pub quit: bool,
    pub title: Option<String>,
    pub resize: Option<(f32, f32)>,
    /// A text control has focus: show the soft keyboard / enable the IME.
    pub text_input: bool,
}

struct ClickState {
    time: Instant,
    pos: (f32, f32),
    count: u32,
}

#[derive(Default)]
struct InputState {
    pos: (f32, f32),
    hover: Option<NodeId>,
    pressed: Option<NodeId>,
    buttons: u32,
    mods: Mods,
    last_click: Option<ClickState>,
    /// Text-control drag selection: (control, anchor byte offset).
    drag: Option<(NodeId, usize)>,
    /// A text control with edits not yet announced with a `change` event.
    edit_dirty: Option<NodeId>,
    space_down_on: Option<NodeId>,
    window_focused: bool,
    /// Active touch points (multi-touch), oldest first.
    touches: Vec<TouchPoint>,
    /// While a touch gesture synthesises compatibility mouse events, `pointerType` is `"touch"`.
    touch_mode: bool,
    /// Inertial scroll after a flick.
    fling: Option<Fling>,
    /// Connected gamepads (index → state); see [`Session::set_gamepad`].
    gamepads: Vec<Option<GamepadState>>,
}

/// Finger travel (CSS px) before a touch stops being a tap and becomes a pan.
const TOUCH_SLOP: f32 = 10.0;
/// A stationary touch held this long fires `contextmenu` (long press).
const LONG_PRESS: std::time::Duration = std::time::Duration::from_millis(500);

struct TouchPoint {
    id: u32,
    start: (f32, f32),
    pos: (f32, f32),
    target: Option<NodeId>,
    t0: Instant,
    last_t: Instant,
    /// Smoothed finger velocity in px/s.
    vel: (f32, f32),
    panning: bool,
    /// `pointercancel` was sent (the browser took the gesture for scrolling): no more pointer events.
    pointer_cancelled: bool,
    /// `touchstart` (or a later `touchmove`) called `preventDefault`: no scroll, no compat mouse events.
    prevented: bool,
    long_press_fired: bool,
    /// Pan allowed by `touch-action` at the touch target: bit 1 = x, 2 = y.
    pan_mask: u8,
}

struct Fling {
    pos: (f32, f32),
    vel: (f32, f32),
    last: Instant,
}

/// One gamepad in the W3C "standard" layout: 17 buttons (value 0..1) and 4 axes (-1..1).
#[derive(Debug, Clone, PartialEq)]
pub struct GamepadState {
    pub id: String,
    pub buttons: Vec<f32>,
    pub axes: Vec<f32>,
}

pub struct Session {
    pub engine: Rc<RefCell<Engine>>,
    pub js: JsRuntime,
    input: InputState,
    clipboard: Box<dyn Clipboard>,
    last_cursor: Option<Cursor>,
    title: String,
    pub bundle_name: String,
}

fn normalize(p: &str) -> String {
    p.trim_start_matches("./").trim_start_matches('/').split(['?', '#']).next().unwrap_or("").to_string()
}

impl Drop for Session {
    // A4 workers: bounded shutdown (never deadlocks the close path).
    fn drop(&mut self) {
        self.js.shutdown_workers();
    }
}

impl Session {
    /// Parse the bundle's document, install the runtime and run its scripts in document order.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn new(bundle: &AppBundle, clipboard: Box<dyn Clipboard>, viewport: (f32, f32, f32)) -> Result<Session, String> {
        Self::new_with_engine(bundle, clipboard, viewport, |host| Ok(Box::new(rendrix_js::quickjs::QuickJsEngine::new(host)?) as Box<dyn rendrix_js::JsEngine>))
    }

    /// Like [`Session::new`] on a caller-supplied JavaScript engine (the web driver passes the
    /// browser's). `make_engine` receives the host the engine routes `__native` calls to.
    pub fn new_with_engine(
        bundle: &AppBundle,
        clipboard: Box<dyn Clipboard>,
        viewport: (f32, f32, f32),
        make_engine: impl FnOnce(Rc<RefCell<rendrix_js::DomHost>>) -> Result<Box<dyn rendrix_js::JsEngine>, rendrix_js::JsError>,
    ) -> Result<Session, String> {
        let doc = rendrix_dom::Document::parse(&bundle.html);
        let mut engine = Engine::with_document(doc, bundle.asset_provider());
        engine.set_viewport(viewport.0, viewport.1, viewport.2);
        let engine = Rc::new(RefCell::new(engine));
        let mut js = JsRuntime::new_with_engine(engine.clone(), &bundle.name, bundle.permissions(), make_engine).map_err(|e| e.to_string())?;
        for (path, src) in &bundle.modules {
            js.add_module(path, src.clone());
        }
        for (path, map) in &bundle.source_maps {
            js.add_source_map(path, &map.to_string());
        }
        let mut session = Session { engine, js, input: InputState { window_focused: true, ..Default::default() }, clipboard, last_cursor: None, title: bundle.window_title(), bundle_name: bundle.name.clone() };
        let script_start = Instant::now();
        session.run_document_scripts(bundle)?;
        SCRIPT_RUN_NANOS.with(|c| c.set(script_start.elapsed().as_nanos() as u64));
        // A4 workers: hand the bundle module map + network grant to the worker registry.
        session.js.set_worker_modules(bundle.modules.iter().map(|(k, v)| (k.clone(), v.clone())).collect());
        session.js.set_worker_network(bundle.permissions.network);
        session.js.boot();
        Ok(session)
    }

    /// D2 hot swap: validate + commit `changed` (the full bundle.modules map) without
    /// dropping the DOM. Returns re-run entry labels; on Err the session is last-good.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn apply_hot_update(&mut self, changed: std::collections::BTreeMap<String, String>) -> Result<Vec<String>, String> {
        crate::hot_reload::apply_module_update(self, changed)
    }

    fn run_document_scripts(&mut self, bundle: &AppBundle) -> Result<(), String> {
        // Collect first: scripts may mutate the document while they run.
        let scripts: Vec<(Option<String>, String, bool)> = {
            let e = self.engine.borrow();
            let doc = &e.doc;
            doc.get_elements_by_tag_name(doc.root(), "script")
                .into_iter()
                .map(|n| (doc.get_attr(n, "src").map(str::to_string), doc.text_content(n), doc.get_attr(n, "type").is_some_and(|t| t.eq_ignore_ascii_case("module"))))
                .collect()
        };
        let mut inline = 0;
        for (src, text, is_module) in scripts {
            match (src, is_module) {
                (Some(src), true) => {
                    let key = normalize(&src);
                    let name = [key.clone(), format!("{key}.js")].into_iter().find(|k| bundle.modules.contains_key(k)).ok_or_else(|| format!("module script '{src}' is not in the bundle"))?;
                    self.js.run_module(&name).map_err(|e| format!("{src}: {e}"))?;
                }
                (None, true) => {
                    inline += 1;
                    let name = format!("inline-{inline}.js");
                    self.js.add_module(&name, text);
                    self.js.run_module(&name).map_err(|e| format!("{name}: {e}"))?;
                }
                (Some(src), false) => {
                    let key = normalize(&src);
                    let code = bundle.modules.get(&key).cloned().or_else(|| self.engine.borrow().read_asset(&key).map(|b| String::from_utf8_lossy(&b).into_owned())).ok_or_else(|| format!("script '{src}' is not in the bundle"))?;
                    self.js.run_script(&src, &code).map_err(|e| format!("{src}: {e}"))?;
                }
                (None, false) => {
                    if !text.trim().is_empty() {
                        self.js.run_script("inline-script", &text).map_err(|e| format!("inline script: {e}"))?;
                    }
                }
            }
        }
        Ok(())
    }

    pub fn now_s(&self) -> f64 {
        self.js.host.borrow().now_ms() / 1000.0
    }

    pub fn resize(&mut self, width: f32, height: f32, scale: f32) {
        self.engine.borrow_mut().set_viewport(width, height, scale);
        self.js.viewport_changed();
    }

    // ------------------------------------------------------------------------------------
    // Event plumbing
    // ------------------------------------------------------------------------------------

    fn base_init(&self, extra: Value) -> Value {
        let m = self.input.mods;
        let mut v = json!({
            "clientX": self.input.pos.0, "clientY": self.input.pos.1, "pageX": self.input.pos.0, "pageY": self.input.pos.1, "screenX": self.input.pos.0, "screenY": self.input.pos.1,
            "buttons": self.input.buttons, "pointerType": if self.input.touch_mode { "touch" } else { "mouse" }, "ctrlKey": m.ctrl, "shiftKey": m.shift, "altKey": m.alt, "metaKey": m.meta,
        });
        if let (Value::Object(base), Value::Object(more)) = (&mut v, extra) {
            base.extend(more);
        }
        v
    }

    fn dispatch(&mut self, ty: &str, target: Option<NodeId>, extra: Value) -> bool {
        let init = self.base_init(extra);
        self.js.dispatch(ty, target, &init)
    }

    fn elements_on_path(&self, id: Option<NodeId>) -> Vec<NodeId> {
        let e = self.engine.borrow();
        match id {
            Some(n) => e.doc.event_path(n).into_iter().filter(|x| matches!(e.doc.node(*x).kind, NodeKind::Element(_))).collect(),
            None => vec![],
        }
    }

    fn set_hover(&mut self, new: Option<NodeId>) {
        let old = self.input.hover;
        if old == new {
            return;
        }
        let (old_path, new_path) = (self.elements_on_path(old), self.elements_on_path(new));
        let rel_new = new.map(|n| json!({ "relatedId": n })).unwrap_or(json!({}));
        let rel_old = old.map(|n| json!({ "relatedId": n })).unwrap_or(json!({}));
        if let Some(o) = old {
            self.dispatch("pointerout", Some(o), rel_new.clone());
            self.dispatch("mouseout", Some(o), rel_new.clone());
            for n in old_path.iter().filter(|n| !new_path.contains(n)) {
                self.dispatch("pointerleave", Some(*n), rel_new.clone());
                self.dispatch("mouseleave", Some(*n), rel_new.clone());
            }
        }
        self.engine.borrow_mut().doc.set_hover_chain(new);
        self.input.hover = new;
        if let Some(n) = new {
            self.dispatch("pointerover", Some(n), rel_old.clone());
            self.dispatch("mouseover", Some(n), rel_old.clone());
            for e in new_path.iter().rev().filter(|e| !old_path.contains(e)) {
                self.dispatch("pointerenter", Some(*e), rel_old.clone());
                self.dispatch("mouseenter", Some(*e), rel_old.clone());
            }
        }
    }

    pub fn pointer_move(&mut self, x: f32, y: f32) {
        self.pointer_move_inner(x, y, true);
    }

    fn pointer_move_inner(&mut self, x: f32, y: f32, emit_pointer: bool) {
        self.input.pos = (x, y);
        let target = self.engine.borrow().hit_test(x, y);
        self.set_hover(target);
        if let Some(t) = target {
            if emit_pointer {
                self.dispatch("pointermove", Some(t), json!({}));
            }
            self.dispatch("mousemove", Some(t), json!({}));
        }
        if let Some((c, anchor)) = self.input.drag {
            let off = self.engine.borrow().text_offset_at(c, x, y);
            if let Some(off) = off {
                self.engine.borrow_mut().doc.set_selection(c, anchor, off);
                self.engine.borrow_mut().reset_caret();
            }
        }
    }

    fn focus_target(&self, mut n: NodeId) -> Option<NodeId> {
        let e = self.engine.borrow();
        loop {
            if e.doc.is_focusable(n) {
                return Some(n);
            }
            match e.doc.parent(n) {
                Some(p) if e.doc.element(p).is_some() => n = p,
                _ => return None,
            }
        }
    }

    fn move_focus(&mut self, to: Option<NodeId>) {
        let current = self.engine.borrow().doc.focused();
        if current == to {
            return;
        }
        if let Some(dirty) = self.input.edit_dirty.take() {
            if Some(dirty) != to {
                self.js.fire_change(dirty);
            }
        }
        self.js.set_focus(to);
    }

    pub fn pointer_down(&mut self, x: f32, y: f32, button: u32) {
        self.input.fling = None;
        self.pointer_down_inner(x, y, button, true);
    }

    fn pointer_down_inner(&mut self, x: f32, y: f32, button: u32, emit_pointer: bool) {
        self.input.pos = (x, y);
        let target = self.engine.borrow().hit_test(x, y);
        self.set_hover(target);
        let bit = match button {
            0 => 1,
            1 => 4,
            2 => 2,
            _ => 0,
        };
        self.input.buttons |= bit;
        let Some(t) = target else {
            self.move_focus(None);
            return;
        };
        if button == 0 {
            self.engine.borrow_mut().doc.set_active_chain(Some(t));
            self.input.pressed = Some(t);
        }
        let ok_p = !emit_pointer || self.dispatch("pointerdown", Some(t), json!({ "button": button }));
        let ok_m = self.dispatch("mousedown", Some(t), json!({ "button": button }));
        if !(ok_p && ok_m) || button != 0 {
            return;
        }
        // Click counting for double/triple click selection.
        let now = Instant::now();
        let count = match &self.input.last_click {
            Some(c) if now.duration_since(c.time).as_millis() < 450 && (c.pos.0 - x).abs() < 5.0 && (c.pos.1 - y).abs() < 5.0 => c.count + 1,
            _ => 1,
        };
        self.input.last_click = Some(ClickState { time: now, pos: (x, y), count });
        // Pointer focus suppresses :focus-visible (keyboard focus shows it).
        self.engine.borrow_mut().doc.focus_modality_keyboard = false;
        let f = self.focus_target(t);
        self.move_focus(f);
        if let Some(f) = f {
            let is_text = self.engine.borrow().doc.is_text_control(f);
            if is_text {
                let off = self.engine.borrow().text_offset_at(f, x, y).unwrap_or(0);
                let value = self.engine.borrow().doc.value(f);
                let (a, b) = match count {
                    1 => (off, off),
                    2 => word_bounds(&value, off),
                    _ => (0, value.len()),
                };
                self.engine.borrow_mut().doc.set_selection(f, a, b);
                self.engine.borrow_mut().reset_caret();
                self.input.drag = if count == 1 { Some((f, off)) } else { None };
            }
        }
    }

    pub fn pointer_up(&mut self, x: f32, y: f32, button: u32) {
        self.pointer_up_inner(x, y, button, true);
    }

    fn pointer_up_inner(&mut self, x: f32, y: f32, button: u32, emit_pointer: bool) {
        self.input.pos = (x, y);
        let target = self.engine.borrow().hit_test(x, y);
        let bit = match button {
            0 => 1,
            1 => 4,
            2 => 2,
            _ => 0,
        };
        self.input.buttons &= !bit;
        self.input.drag = None;
        if let Some(t) = target {
            if emit_pointer {
                self.dispatch("pointerup", Some(t), json!({ "button": button }));
            }
            self.dispatch("mouseup", Some(t), json!({ "button": button }));
        }
        if button == 0 {
            self.engine.borrow_mut().doc.set_active_chain(None);
            let pressed = self.input.pressed.take();
            if let (Some(p), Some(t)) = (pressed, target) {
                // `click` goes to the nearest common ancestor of the press and release targets.
                let common = {
                    let e = self.engine.borrow();
                    let pa = e.doc.event_path(p);
                    e.doc.event_path(t).into_iter().find(|n| pa.contains(n) && e.doc.element(*n).is_some())
                };
                if let Some(c) = common {
                    let count = self.input.last_click.as_ref().map(|c| c.count).unwrap_or(1);
                    self.dispatch("click", Some(c), json!({ "button": 0, "detail": count  }));
                    if count == 2 {
                        self.dispatch("dblclick", Some(c), json!({ "button": 0, "detail": 2 }));
                    }
                }
            }
        } else if button == 2 {
            if let Some(t) = target {
                self.dispatch("contextmenu", Some(t), json!({ "button": 2 }));
            }
        } else if button == 1 {
            if let Some(t) = target {
                self.dispatch("auxclick", Some(t), json!({ "button": 1 }));
            }
        }
    }

    pub fn wheel(&mut self, x: f32, y: f32, dx: f32, dy: f32) {
        self.input.fling = None;
        self.input.pos = (x, y);
        let target = self.engine.borrow().hit_test(x, y).or_else(|| self.engine.borrow().doc.body());
        let allowed = self.dispatch("wheel", target, json!({ "deltaX": dx, "deltaY": dy, "deltaMode": 0 }));
        if allowed {
            let scrolled = self.engine.borrow_mut().scroll_by(x, y, dx, dy);
            if scrolled {
                let t = self.engine.borrow().hit_test(x, y);
                if let Some(t) = t {
                    // Scroll changed what is under the pointer.
                    self.set_hover(Some(t));
                }
            }
        }
    }

    // ------------------------------------------------------------------------------------
    // Touch (phones, tablets, touch screens)
    //
    // Mirrors what a mobile browser does: `pointerdown/move/up` (pointerType "touch") and
    // `touchstart/move/end` while the finger is down; a drag past the slop scrolls the nearest
    // scroller (with a flick fling) and sends `pointercancel`; a tap synthesises the compatibility
    // `mousemove/mousedown/mouseup/click` sequence; a long press fires `contextmenu`. Hover never
    // sticks after a tap. `touch-action` limits which axes may pan; calling `preventDefault` on
    // `touchstart`/`touchmove` (non-passive listeners) disables scrolling and compat mouse events.
    // ------------------------------------------------------------------------------------

    /// Effective `touch-action` pan mask at a node: the intersection along the ancestor chain.
    fn pan_mask_at(&self, n: Option<NodeId>) -> u8 {
        let e = self.engine.borrow();
        let mut mask = 7u8;
        let mut cur = n;
        while let Some(id) = cur {
            if let Some(s) = e.style_of(id) {
                mask &= s.touch_action;
            }
            cur = e.doc.parent(id);
        }
        mask & 3
    }

    fn touch_json(&self, tp: &TouchPoint) -> Value {
        json!({ "identifier": tp.id, "clientX": tp.pos.0, "clientY": tp.pos.1, "pageX": tp.pos.0, "pageY": tp.pos.1, "screenX": tp.pos.0, "screenY": tp.pos.1, "targetId": tp.target })
    }

    /// Dispatch a `touch*` event: `changed` are the touch ids that changed in this event.
    fn dispatch_touch(&mut self, ty: &str, target: Option<NodeId>, changed: &[u32], gone: Option<&TouchPoint>) -> bool {
        let live: Vec<Value> = self.input.touches.iter().map(|t| self.touch_json(t)).collect();
        let on_target: Vec<Value> = self.input.touches.iter().filter(|t| t.target == target).map(|t| self.touch_json(t)).collect();
        let mut changed_json: Vec<Value> = self.input.touches.iter().filter(|t| changed.contains(&t.id)).map(|t| self.touch_json(t)).collect();
        if let Some(g) = gone {
            changed_json.push(self.touch_json(g));
        }
        let init = self.base_init(json!({ "touches": live, "targetTouches": on_target, "changedTouches": changed_json }));
        self.js.dispatch(ty, target, &init)
    }

    pub fn touch_start(&mut self, id: u32, x: f32, y: f32) {
        self.input.fling = None;
        self.input.pos = (x, y);
        self.input.touch_mode = true;
        let target = self.engine.borrow().hit_test(x, y);
        let primary = self.input.touches.is_empty();
        if primary {
            self.set_hover(target);
        }
        let now = Instant::now();
        let pan_mask = self.pan_mask_at(target);
        self.input.touches.push(TouchPoint { id, start: (x, y), pos: (x, y), target, t0: now, last_t: now, vel: (0.0, 0.0), panning: false, pointer_cancelled: false, prevented: false, long_press_fired: false, pan_mask });
        self.input.buttons |= 1;
        if let Some(t) = target {
            if primary {
                self.engine.borrow_mut().doc.set_active_chain(Some(t));
                self.input.pressed = Some(t);
            }
            self.dispatch("pointerdown", Some(t), json!({ "button": 0, "pointerId": id + 2, "isPrimary": primary }));
            if !self.dispatch_touch("touchstart", Some(t), &[id], None) {
                if let Some(tp) = self.input.touches.iter_mut().find(|t| t.id == id) {
                    tp.prevented = true;
                }
            }
        }
        self.input.touch_mode = false;
    }

    pub fn touch_move(&mut self, id: u32, x: f32, y: f32) {
        let Some(i) = self.input.touches.iter().position(|t| t.id == id) else { return };
        self.input.pos = (x, y);
        let now = Instant::now();
        let (prev, target, primary) = {
            let tp = &mut self.input.touches[i];
            let dt = now.duration_since(tp.last_t).as_secs_f32().max(0.001);
            let inst = ((x - tp.pos.0) / dt, (y - tp.pos.1) / dt);
            // Exponential smoothing keeps a final slow-down from being lost in a single noisy sample.
            tp.vel = if tp.vel == (0.0, 0.0) { inst } else { (tp.vel.0 * 0.4 + inst.0 * 0.6, tp.vel.1 * 0.4 + inst.1 * 0.6) };
            let prev = tp.pos;
            tp.pos = (x, y);
            tp.last_t = now;
            (prev, tp.target, i == 0)
        };
        if (x, y) == prev {
            return;
        }
        self.input.touch_mode = true;
        let (cancelled, panning) = {
            let tp = &self.input.touches[i];
            (tp.pointer_cancelled, tp.panning)
        };
        if let Some(t) = target {
            if !cancelled {
                self.dispatch("pointermove", Some(t), json!({ "pointerId": id + 2, "isPrimary": primary }));
            }
            if !self.dispatch_touch("touchmove", Some(t), &[id], None) {
                self.input.touches[i].prevented = true;
            }
        }
        let (prevented, start, pan_mask) = {
            let tp = &self.input.touches[i];
            (tp.prevented, tp.start, tp.pan_mask)
        };
        if !prevented {
            if !panning && pan_mask != 0 && ((x - start.0).powi(2) + (y - start.1).powi(2)).sqrt() > TOUCH_SLOP {
                self.input.touches[i].panning = true;
                if let Some(t) = target {
                    if !cancelled {
                        self.dispatch("pointercancel", Some(t), json!({ "pointerId": id + 2, "isPrimary": primary }));
                    }
                }
                self.input.touches[i].pointer_cancelled = true;
                self.engine.borrow_mut().doc.set_active_chain(None);
                self.input.pressed = None;
            }
            if self.input.touches[i].panning && i == 0 {
                let dx = if pan_mask & 1 != 0 { prev.0 - x } else { 0.0 };
                let dy = if pan_mask & 2 != 0 { prev.1 - y } else { 0.0 };
                if dx != 0.0 || dy != 0.0 {
                    self.engine.borrow_mut().scroll_by(x, y, dx, dy);
                }
            }
        }
        self.input.touch_mode = false;
    }

    pub fn touch_end(&mut self, id: u32, x: f32, y: f32) {
        self.touch_finish(id, x, y, false);
    }

    /// The system took the touch (palm rejection, gesture, app switch): no tap, no fling.
    pub fn touch_cancel(&mut self, id: u32) {
        let pos = self.input.touches.iter().find(|t| t.id == id).map(|t| t.pos);
        if let Some((x, y)) = pos {
            self.touch_finish(id, x, y, true);
        }
    }

    fn touch_finish(&mut self, id: u32, x: f32, y: f32, cancelled: bool) {
        let Some(i) = self.input.touches.iter().position(|t| t.id == id) else { return };
        let mut tp = self.input.touches.remove(i);
        tp.pos = (x, y);
        self.input.pos = (x, y);
        self.input.touch_mode = true;
        let primary = i == 0;
        if let Some(t) = tp.target {
            let ty = if cancelled { "pointercancel" } else { "pointerup" };
            if !tp.pointer_cancelled {
                self.dispatch(ty, Some(t), json!({ "button": 0, "pointerId": id + 2, "isPrimary": primary }));
            }
        }
        let touchend_ok = match tp.target {
            Some(t) => self.dispatch_touch(if cancelled { "touchcancel" } else { "touchend" }, Some(t), &[], Some(&tp)),
            None => true,
        };
        let last = self.input.touches.is_empty();
        let moved = ((x - tp.start.0).powi(2) + (y - tp.start.1).powi(2)).sqrt() > TOUCH_SLOP;
        if last {
            self.input.buttons &= !1;
            self.engine.borrow_mut().doc.set_active_chain(None);
            self.input.pressed = None;
        }
        let is_tap = !cancelled && last && !tp.panning && !tp.prevented && !tp.long_press_fired && touchend_ok && !moved;
        if is_tap {
            // Compatibility mouse events: `mousemove`, `mousedown`, `mouseup`, `click` (pointerType touch).
            self.pointer_move_inner(x, y, false);
            self.pointer_down_inner(x, y, 0, false);
            self.pointer_up_inner(x, y, 0, false);
        } else if !cancelled && last && tp.panning && !tp.prevented {
            let v = tp.vel;
            // A finger that stopped before lifting must not fling: require a recent movement sample.
            let fresh = Instant::now().duration_since(tp.last_t).as_millis() < 80;
            let (vx, vy) = (if tp.pan_mask & 1 != 0 { v.0 } else { 0.0 }, if tp.pan_mask & 2 != 0 { v.1 } else { 0.0 });
            if fresh && (vx * vx + vy * vy).sqrt() > 120.0 {
                self.input.fling = Some(Fling { pos: (x, y), vel: (vx.clamp(-6000.0, 6000.0), vy.clamp(-6000.0, 6000.0)), last: Instant::now() });
            }
        }
        if last {
            self.set_hover(None);
        }
        self.input.touch_mode = false;
    }

    /// Whether a finger is currently down (drivers keep the frame loop awake for flings/long press).
    pub fn touching(&self) -> bool {
        !self.input.touches.is_empty()
    }

    fn step_fling(&mut self) -> bool {
        let Some(f) = self.input.fling.as_mut() else { return false };
        let now = Instant::now();
        let dt = now.duration_since(f.last).as_secs_f32().clamp(0.0, 0.05);
        f.last = now;
        // Finger-velocity to content delta is the inverse sign; friction is about 0.95 per 60 Hz frame.
        let (dx, dy) = (-f.vel.0 * dt, -f.vel.1 * dt);
        let decay = (-3.0 * dt).exp();
        f.vel = (f.vel.0 * decay, f.vel.1 * decay);
        let (pos, slow) = (f.pos, (f.vel.0 * f.vel.0 + f.vel.1 * f.vel.1).sqrt() < 20.0);
        let moved = self.engine.borrow_mut().scroll_by(pos.0, pos.1, dx, dy);
        if !moved || slow {
            self.input.fling = None;
            return false;
        }
        true
    }

    /// Fire `contextmenu` for touches held past [`LONG_PRESS`]. Returns time until the next one is due.
    fn step_long_press(&mut self) -> Option<std::time::Duration> {
        let now = Instant::now();
        let mut due: Vec<Option<NodeId>> = Vec::new();
        let mut next: Option<std::time::Duration> = None;
        for tp in self.input.touches.iter_mut() {
            if tp.panning || tp.prevented || tp.long_press_fired {
                continue;
            }
            if ((tp.pos.0 - tp.start.0).powi(2) + (tp.pos.1 - tp.start.1).powi(2)).sqrt() > TOUCH_SLOP {
                continue;
            }
            let held = now.duration_since(tp.t0);
            if held >= LONG_PRESS {
                tp.long_press_fired = true;
                due.push(tp.target);
            } else {
                let left = LONG_PRESS - held;
                next = Some(next.map_or(left, |n| n.min(left)));
            }
        }
        for target in due.into_iter().flatten() {
            self.input.touch_mode = true;
            self.dispatch("contextmenu", Some(target), json!({ "button": 2 }));
            self.input.touch_mode = false;
        }
        next
    }

    // ------------------------------------------------------------------------------------
    // Safe area, on-screen keyboard, gamepads
    // ------------------------------------------------------------------------------------

    /// Notch / system-bar insets in CSS px, exposed to CSS as `env(safe-area-inset-*)`.
    pub fn set_safe_area(&mut self, top: f32, right: f32, bottom: f32, left: f32) {
        self.engine.borrow_mut().set_safe_area([top.max(0.0), right.max(0.0), bottom.max(0.0), left.max(0.0)]);
    }

    /// On-screen keyboard height in CSS px (`env(keyboard-inset-height)`); 0 when hidden. When it
    /// opens over a focused text control the control is scrolled into view.
    pub fn set_keyboard_height(&mut self, height: f32) {
        let was = self.engine.borrow().keyboard_h;
        self.engine.borrow_mut().set_keyboard_inset(height.max(0.0));
        if height > 0.0 && was <= 0.0 && self.wants_text_input() {
            let _ = self.js.eval("document.activeElement && document.activeElement.scrollIntoView && document.activeElement.scrollIntoView()");
        }
    }

    /// A text control has focus: drivers should show the soft keyboard / enable the IME.
    pub fn wants_text_input(&self) -> bool {
        let e = self.engine.borrow();
        e.doc.focused().is_some_and(|n| e.doc.is_text_control(n))
    }

    /// `navigator.maxTouchPoints` (0 = no touch screen).
    pub fn set_max_touch_points(&mut self, n: u32) {
        self.js.host.borrow_mut().max_touch_points = n;
    }

    /// Report the current state of gamepad `index` (`None` = disconnected). Drivers call this every
    /// frame with whatever the OS/browser reports, in the W3C standard layout; the session turns
    /// changes into `gamepadconnected`/`gamepaddisconnected` and keeps `navigator.getGamepads()` current.
    pub fn set_gamepad(&mut self, index: usize, state: Option<GamepadState>) {
        if self.input.gamepads.len() <= index {
            self.input.gamepads.resize(index + 1, None);
        }
        let prev = self.input.gamepads[index].clone();
        if prev == state {
            return;
        }
        self.input.gamepads[index] = state.clone();
        let json_list: Vec<Value> = self.input.gamepads.iter().enumerate().filter_map(|(i, g)| g.as_ref().map(|g| gamepad_json(i, g))).collect();
        self.js.host.borrow_mut().gamepads_json = Value::Array(json_list).to_string();
        match (&prev, &state) {
            (None, Some(g)) => {
                self.js.dispatch("gamepadconnected", None, &json!({ "gamepad": gamepad_json(index, g) }));
            }
            (Some(g), None) => {
                let mut gone = gamepad_json(index, g);
                gone["connected"] = json!(false);
                self.js.dispatch("gamepaddisconnected", None, &json!({ "gamepad": gone }));
            }
            _ => {}
        }
    }

    // ------------------------------------------------------------------------------------
    // Keyboard
    // ------------------------------------------------------------------------------------

    pub fn set_mods(&mut self, mods: Mods) {
        self.input.mods = mods;
    }

    fn key_init(&self, k: &KeyInput) -> Value {
        self.base_init(json!({ "key": k.key, "code": k.code, "repeat": k.repeat, "ctrlKey": k.mods.ctrl, "shiftKey": k.mods.shift, "altKey": k.mods.alt, "metaKey": k.mods.meta }))
    }

    fn key_target(&self) -> Option<NodeId> {
        let e = self.engine.borrow();
        e.doc.focused().or_else(|| e.doc.body())
    }

    pub fn key_down(&mut self, k: KeyInput) {
        self.input.mods = k.mods;
        let target = self.key_target();
        let init = self.key_init(&k);
        let allowed = self.js.dispatch("keydown", target, &init);
        if !allowed {
            return;
        }
        self.default_key_action(&k, target);
    }

    pub fn key_up(&mut self, k: KeyInput) {
        self.input.mods = k.mods;
        let target = self.key_target();
        let init = self.key_init(&k);
        let allowed = self.js.dispatch("keyup", target, &init);
        if k.key == " " && allowed {
            if let (Some(t), Some(down)) = (target, self.input.space_down_on.take()) {
                if t == down && self.is_click_on_space(t) {
                    self.dispatch("click", Some(t), json!({ "detail": 0 }));
                }
            }
        }
    }

    fn is_click_on_space(&self, n: NodeId) -> bool {
        let e = self.engine.borrow();
        match e.doc.tag(n) {
            Some("button") => true,
            Some("input") => matches!(e.doc.get_attr(n, "type").map(|t| t.to_ascii_lowercase()).as_deref(), Some("checkbox" | "radio" | "button" | "submit" | "reset")),
            _ => false,
        }
    }

    fn focusables(&self) -> Vec<NodeId> {
        let e = self.engine.borrow();
        let mut positive: Vec<(i32, NodeId)> = Vec::new();
        let mut zero: Vec<NodeId> = Vec::new();
        for n in e.doc.depth_first(e.doc.root()) {
            if !e.doc.is_focusable(n) || e.element_rect(n).is_none() {
                continue;
            }
            match e.doc.get_attr(n, "tabindex").and_then(|t| t.trim().parse::<i32>().ok()) {
                Some(t) if t > 0 => positive.push((t, n)),
                Some(t) if t < 0 => {}
                _ => zero.push(n),
            }
        }
        positive.sort_by_key(|(t, _)| *t);
        positive.into_iter().map(|(_, n)| n).chain(zero).collect()
    }

    fn default_key_action(&mut self, k: &KeyInput, target: Option<NodeId>) {
        let m = k.mods;
        if k.key == "Tab" && !m.ctrl && !m.alt {
            self.engine.borrow_mut().doc.focus_modality_keyboard = true;
            let list = self.focusables();
            if list.is_empty() {
                return;
            }
            let cur = self.engine.borrow().doc.focused();
            let pos = cur.and_then(|c| list.iter().position(|n| *n == c));
            let next = match (pos, m.shift) {
                (Some(i), false) => list[(i + 1) % list.len()],
                (Some(i), true) => list[(i + list.len() - 1) % list.len()],
                (None, false) => list[0],
                (None, true) => list[list.len() - 1],
            };
            self.move_focus(Some(next));
            return;
        }
        let Some(t) = target else { return };
        let (is_text, tag) = {
            let e = self.engine.borrow();
            (e.doc.is_text_control(t) && e.doc.element(t).is_some_and(|el| el.flags.focus), e.doc.tag(t).map(str::to_string))
        };
        if is_text {
            self.text_key(k, t, tag.as_deref() == Some("textarea"));
            return;
        }
        // Select popup keyboard support: arrows/Home/End move selection and fire
        // input+change immediately (no native popup list in Rendrix).
        if tag.as_deref() == Some("select") {
            let step: Option<i32> = match k.key.as_str() {
                "ArrowDown" => Some(1),
                "ArrowUp" => Some(-1),
                "Home" => Some(-1000000),
                "End" => Some(1000000),
                _ => None,
            };
            if let Some(d) = step {
                let changed = {
                    let mut e = self.engine.borrow_mut();
                    // Descendant options in document order.
                    let mut options: Vec<NodeId> = Vec::new();
                    for n in e.doc.depth_first(t) {
                        if e.doc.tag(n) == Some("option") {
                            options.push(n);
                        }
                    }
                    if options.is_empty() {
                        false
                    } else {
                        let cur = options.iter().position(|n| e.doc.has_attr(*n, "selected")).unwrap_or(0);
                        // Skip disabled options.
                        let mut next = cur as i32;
                        if d.abs() >= 1000000 {
                            next = if d < 0 { 0 } else { options.len() as i32 - 1 };
                        } else {
                            for _ in 0..options.len() {
                                next = (next + d).clamp(0, options.len() as i32 - 1);
                                if !e.doc.has_attr(options[next as usize], "disabled") {
                                    break;
                                }
                                if next == 0 || next as usize == options.len() - 1 {
                                    break;
                                }
                            }
                        }
                        let ni = next.clamp(0, options.len() as i32 - 1) as usize;
                        if ni == cur {
                            false
                        } else {
                            for (i, n) in options.iter().enumerate() {
                                if i == ni {
                                    e.doc.set_attr(*n, "selected", "");
                                } else {
                                    e.doc.remove_attr(*n, "selected");
                                }
                            }
                            true
                        }
                    }
                };
                if changed {
                    self.js.fire_input(t, "", None);
                    self.js.fire_change(t);
                }
                if k.key == "ArrowDown" || k.key == "ArrowUp" || k.key == "Home" || k.key == "End" {
                    return;
                }
            }
        }
        // Activation keys on buttons, checkboxes, radios and links.
        match (k.key.as_str(), tag.as_deref()) {
            ("Enter", Some("button" | "a")) => {
                self.dispatch("click", Some(t), json!({ "detail": 0 }));
            }
            ("Enter", Some("input")) => {
                let ty = self.engine.borrow().doc.get_attr(t, "type").map(|x| x.to_ascii_lowercase());
                if matches!(ty.as_deref(), Some("button" | "submit" | "reset" | "checkbox" | "radio")) {
                    self.dispatch("click", Some(t), json!({ "detail": 0 }));
                }
            }
            (" ", Some(_)) if self.is_click_on_space(t) => self.input.space_down_on = Some(t),
            _ => {}
        }
        // Keyboard scrolling of the document / focused scroller.
        let (vw, vh) = self.engine.borrow().viewport;
        let step = match k.key.as_str() {
            "ArrowDown" => Some(40.0),
            "ArrowUp" => Some(-40.0),
            "PageDown" => Some(vh * 0.9),
            "PageUp" => Some(-vh * 0.9),
            " " if !self.is_click_on_space(t) => Some(if m.shift { -vh * 0.9 } else { vh * 0.9 }),
            "Home" => Some(-1.0e9),
            "End" => Some(1.0e9),
            _ => None,
        };
        if let Some(dy) = step {
            self.engine.borrow_mut().scroll_by(vw / 2.0, vh / 2.0, 0.0, dy);
        }
    }

    fn text_key(&mut self, k: &KeyInput, id: NodeId, multiline: bool) {
        let m = k.mods;
        let cmd = m.ctrl || m.meta;
        let word = m.ctrl || m.alt;
        let mut value_change: Option<(&str, Option<String>)> = None;
        let op = match k.key.as_str() {
            "Backspace" => Some((EditOp::Backspace { word }, "deleteContentBackward")),
            "Delete" => Some((EditOp::Delete { word }, "deleteContentForward")),
            "ArrowLeft" => Some((EditOp::Left { shift: m.shift, word }, "")),
            "ArrowRight" => Some((EditOp::Right { shift: m.shift, word }, "")),
            "ArrowUp" => Some((EditOp::Up { shift: m.shift }, "")),
            "ArrowDown" => Some((EditOp::Down { shift: m.shift }, "")),
            "Home" => Some((EditOp::Home { shift: m.shift }, "")),
            "End" => Some((EditOp::End { shift: m.shift }, "")),
            "a" | "A" if cmd => Some((EditOp::SelectAll, "")),
            "z" | "Z" if cmd && !m.shift => Some((EditOp::Undo, "historyUndo")),
            "z" | "Z" if cmd && m.shift => Some((EditOp::Redo, "historyRedo")),
            "y" | "Y" if cmd => Some((EditOp::Redo, "historyRedo")),
            "Enter" if multiline => Some((EditOp::Insert("\n".into()), "insertLineBreak")),
            _ => None,
        };
        if let Some((op, input_type)) = op {
            let inserted = if let EditOp::Insert(s) = &op { Some(s.clone()) } else { None };
            let r = self.engine.borrow_mut().edit(id, op);
            if r.value_changed {
                value_change = Some((input_type, inserted));
            }
        } else if k.key == "Enter" {
            // Single-line field: commit, then implicit form submission.
            if let Some(d) = self.input.edit_dirty.take() {
                self.js.fire_change(d);
            }
            self.js.submit_form(id);
        } else if cmd && matches!(k.key.as_str(), "c" | "C" | "x" | "X") {
            let (text, cut) = {
                let e = self.engine.borrow();
                let Some(el) = e.doc.element(id) else { return };
                let v = e.doc.value(id);
                let (a, b) = (el.form.selection_start.min(el.form.selection_end), el.form.selection_start.max(el.form.selection_end));
                (v.get(a..b).unwrap_or("").to_string(), matches!(k.key.as_str(), "x" | "X"))
            };
            if !text.is_empty() {
                self.clipboard.set(&text);
                self.js.host.borrow_mut().clipboard = text;
                if cut {
                    let r = self.engine.borrow_mut().edit(id, EditOp::Backspace { word: false });
                    if r.value_changed {
                        value_change = Some(("deleteByCut", None));
                    }
                }
            }
        } else if cmd && matches!(k.key.as_str(), "v" | "V") {
            let text = self.clipboard.get();
            if !text.is_empty() {
                let r = self.engine.borrow_mut().edit(id, EditOp::Insert(text.clone()));
                if r.value_changed {
                    value_change = Some(("insertFromPaste", Some(text)));
                }
            }
        } else if let Some(text) = k.text.as_ref().filter(|t| !cmd && !t.chars().any(|c| c.is_control())) {
            let r = self.engine.borrow_mut().edit(id, EditOp::Insert(text.clone()));
            if r.value_changed {
                value_change = Some(("insertText", Some(text.clone())));
            }
        }
        if let Some((ty, data)) = value_change {
            self.input.edit_dirty = Some(id);
            self.js.fire_input(id, ty, data.as_deref());
        }
    }

    /// Committed text from an input method.
    pub fn text_commit(&mut self, text: &str) {
        let Some(id) = self.engine.borrow().doc.focused().filter(|n| self.engine.borrow().doc.is_text_control(*n)) else { return };
        let r = self.engine.borrow_mut().edit(id, EditOp::Insert(text.to_string()));
        if r.value_changed {
            self.input.edit_dirty = Some(id);
            self.js.fire_input(id, "insertCompositionText", Some(text));
        }
    }

    /// Caret rectangle of the focused text control in viewport coordinates (for IME placement).
    pub fn caret_rect(&self) -> Option<[f32; 4]> {
        let e = self.engine.borrow();
        let id = e.doc.focused().filter(|n| e.doc.is_text_control(*n))?;
        let content = e.content_of(id)?;
        let (ox, oy, w) = e.text_origin(id)?;
        let single = e.doc.tag(id) == Some("input");
        let sel = e.doc.element(id)?.form.selection_end;
        let (cx, cy, ch) = e.caret_geometry(&content, if single { None } else { Some(w) }, sel);
        Some([ox + cx, oy + cy, 1.0, ch])
    }

    pub fn set_window_focused(&mut self, focused: bool) {
        self.input.window_focused = focused;
        if !focused {
            self.input.buttons = 0;
            self.input.drag = None;
            self.engine.borrow_mut().doc.set_active_chain(None);
        }
        self.engine.borrow_mut().reset_caret();
    }

    // ------------------------------------------------------------------------------------
    // Frames
    // ------------------------------------------------------------------------------------

    /// Run timers/fetches without producing a frame. Returns when the next timer is due (runtime seconds).
    pub fn pump(&mut self) -> Option<f64> {
        self.js.tick().next_timer_ms.map(|ms| ms / 1000.0)
    }

    /// Produce the next frame: timers, `requestAnimationFrame`, style, layout.
    pub fn frame(&mut self) -> FrameInfo {
        let tick = self.js.tick();
        let now = self.now_s();
        if tick.wants_frame {
            self.js.run_animation_frame(now * 1000.0);
        }
        let flinging = self.step_fling();
        let long_press_due = self.step_long_press();
        let mut info = self.engine.borrow_mut().update(now);
        self.js.after_layout();
        // 3D scenes: prune scenes of dead nodes, re-arm on backing-size changes.
        rendrix_js::scene3d::sync_scenes(&mut self.engine.borrow_mut());
        if self.engine.borrow().has_pending_changes() {
            let mut e = self.engine.borrow_mut();
            let second = e.update(now);
            info.repainted |= second.repainted;
            info.animating |= second.animating;
            info.next_wake = second.next_wake.or(info.next_wake);
        }
        let mut out = FrameInfo { repainted: info.repainted, ..Default::default() };
        let tick = self.js.tick_info();
        out.continuous = info.animating || tick.wants_frame || self.engine.borrow().needs_update() || flinging;
        out.text_input = self.wants_text_input();
        let timer = tick.next_timer_ms.map(|ms| ms / 1000.0);
        out.next_wake_s = match (timer, info.next_wake) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        };
        if let Some(d) = long_press_due {
            out.next_wake_s = Some(out.next_wake_s.map_or(now + d.as_secs_f64(), |w| w.min(now + d.as_secs_f64())));
        }
        if tick.pending_io {
            out.next_wake_s = Some(out.next_wake_s.map_or(now + 0.016, |w| w.min(now + 0.016)));
        }
        // A4 workers: pump threads (capped 16/frame into script), wake soon while spilled.
        let worker_events = self.js.poll_workers();
        let workers_busy = !worker_events.is_empty() || self.js.worker_pending();
        drop(worker_events);
        if workers_busy || self.js.worker_alive() {
            out.next_wake_s = Some(out.next_wake_s.map_or(now + 0.016, |w| w.min(now + 0.016)));
        }
        // Cursor from the element under the pointer.
        let c = self.engine.borrow().cursor_at(self.input.pos.0, self.input.pos.1);
        if self.last_cursor != Some(c) {
            self.last_cursor = Some(c);
            out.cursor = Some(c);
        }
        for cmd in self.js.take_window_commands() {
            match cmd {
                WindowCommand::SetTitle(t) => {
                    self.title = t.clone();
                    out.title = Some(t);
                }
                WindowCommand::Resize(w, h) => out.resize = Some((w, h)),
                WindowCommand::Close | WindowCommand::Quit => out.quit = true,
            }
        }
        out
    }

    pub fn title(&self) -> &str {
        &self.title
    }

    pub fn display_list(&self) -> std::cell::Ref<'_, Engine> {
        self.engine.borrow()
    }

    /// First element matching a selector (for scripted drivers).
    pub fn query(&self, selector: &str) -> Option<NodeId> {
        let e = self.engine.borrow();
        e.doc.query_selector(e.doc.root(), selector).ok().flatten()
    }

    /// Centre of an element in viewport coordinates.
    pub fn center_of(&self, id: NodeId) -> Option<(f32, f32)> {
        self.engine.borrow().element_rect(id).map(|r| (r[0] + r[2] / 2.0, r[1] + r[3] / 2.0))
    }
}

/// A gamepad in the JSON shape `navigator.getGamepads()` consumes.
fn gamepad_json(index: usize, g: &GamepadState) -> Value {
    json!({ "index": index, "id": g.id, "connected": true, "mapping": "standard", "buttons": g.buttons, "axes": g.axes, "timestamp": 0 })
}

fn word_bounds(s: &str, at: usize) -> (usize, usize) {
    let at = at.min(s.len());
    let is_word = |c: char| c.is_alphanumeric() || c == '_';
    let mut start = at;
    while start > 0 {
        let prev = s[..start].chars().next_back().unwrap();
        if !is_word(prev) {
            break;
        }
        start -= prev.len_utf8();
    }
    let mut end = at;
    while let Some(c) = s[end..].chars().next() {
        if !is_word(c) {
            break;
        }
        end += c.len_utf8();
    }
    (start, end)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn word_selection_bounds() {
        assert_eq!(word_bounds("hello big world", 7), (6, 9));
        assert_eq!(word_bounds("hello", 0), (0, 5));
        assert_eq!(word_bounds("a  b", 2), (2, 2));
    }
}
