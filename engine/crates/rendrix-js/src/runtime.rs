//! The JavaScript runtime: prelude installation, module loading, and the task/event loop glue.

use crate::engine_trait::{JsEngine, JsError, Val};
use crate::host::{base64_encode, DomHost, Permissions, WindowCommand};
#[cfg(not(target_arch = "wasm32"))]
use crate::quickjs::QuickJsEngine;
use crate::sourcemap::{remap_stack, SourceMap};
use rendrix_core::Engine;
use rendrix_dom::NodeId;
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::time::Duration;

#[cfg(not(target_arch = "wasm32"))]
#[path = "worker.rs"]
mod worker;
#[cfg(target_arch = "wasm32")]
#[path = "worker_stub.rs"]
mod worker;
pub use worker::{WorkerEvent, WorkerRegistry, WORKER_MAX_PER_TICK};

const EVENTS_JS: &str = include_str!("prelude/events.js");
const DOM_JS: &str = include_str!("prelude/dom.js");
const WEB_JS: &str = include_str!("prelude/web.js");
const JSX_RUNTIME_JS: &str = include_str!("prelude/jsx-runtime.js");

/// What the platform layer should do next.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct TickInfo {
    /// `requestAnimationFrame` callbacks are waiting for the next frame.
    pub wants_frame: bool,
    /// Milliseconds (runtime clock) when the next timer is due.
    pub next_timer_ms: Option<f64>,
    /// A fetch is in flight (poll again soon).
    pub pending_io: bool,
}

pub struct JsRuntime {
    pub engine: Rc<RefCell<Engine>>,
    pub host: Rc<RefCell<DomHost>>,
    js: Box<dyn JsEngine>,
    /// Uncaught errors and diagnostics collected since the last `take_errors`.
    errors: Vec<String>,
    /// Wall-clock budget armed around every task (script eval, timer, rAF callback, microtask
    /// drain). Expiry preempts the script via the engine interrupt handler (L4 runaway guard).
    task_budget: Duration,
    /// Bundler-emitted source maps by module key, for remapping error stacks to original sources.
    maps: HashMap<String, SourceMap>,
    /// Dedicated workers (A4): per-worker threads + the event channel into the frame pump.
    workers: WorkerRegistry,
}

/// Default per-task execution budget: generous enough to never trip on legitimate work, tight
/// enough to keep a runaway script from hanging the frame loop.
pub const DEFAULT_TASK_BUDGET: Duration = Duration::from_secs(5);

impl JsRuntime {
    /// Native runtime on the bundled QuickJS engine.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn new(engine: Rc<RefCell<Engine>>, app_name: &str, permissions: Permissions) -> Result<JsRuntime, JsError> {
        Self::new_with_engine(engine, app_name, permissions, |host| Ok(Box::new(QuickJsEngine::new(host)?) as Box<dyn JsEngine>))
    }

    /// Runtime on any [`JsEngine`]: `make_engine` receives the host the engine must route
    /// `__native(op, ...args)` calls to (the browser backend uses this on the web target).
    pub fn new_with_engine(
        engine: Rc<RefCell<Engine>>,
        app_name: &str,
        permissions: Permissions,
        make_engine: impl FnOnce(Rc<RefCell<DomHost>>) -> Result<Box<dyn JsEngine>, JsError>,
    ) -> Result<JsRuntime, JsError> {
        let host = Rc::new(RefCell::new(DomHost::new(engine.clone(), app_name, permissions)));
        let mut js: Box<dyn JsEngine> = make_engine(host.clone())?;
        let prelude = format!("(function(global){{'use strict';\nconst N = global.__native;\ndelete global.__native;\n{EVENTS_JS}\n{DOM_JS}\n{WEB_JS}\n}})(globalThis);");
        js.eval_script("rendrix:prelude", &prelude)?;
        js.add_module("rendrix", "export default globalThis.rendrix; export const { version, quit, window, fs, clipboard } = globalThis.rendrix;".to_string());
        js.add_module("rendrix/jsx-runtime", JSX_RUNTIME_JS.to_string());
        js.add_module("rendrix/jsx-dev-runtime", JSX_RUNTIME_JS.to_string());
        Ok(JsRuntime { engine, host, js, errors: Vec::new(), task_budget: DEFAULT_TASK_BUDGET, maps: HashMap::new(), workers: WorkerRegistry::new() })
    }

    /// Register a bundler-emitted source map for the module under `name`, so error stacks that
    /// reference the compiled file remap to the original source. Invalid maps are ignored —
    /// remapping must never break error reporting.
    pub fn add_source_map(&mut self, name: &str, json: &str) {
        if let Ok(map) = SourceMap::parse(json) {
            self.maps.insert(name.to_string(), map);
        }
    }

    /// Remap an error's stack frames through the registered bundle source maps.
    fn remap_error(&self, mut e: JsError) -> JsError {
        if let Some(stack) = e.stack.take() {
            e.stack = Some(remap_stack(&self.maps, &stack));
        }
        e
    }

    /// Override the per-task execution budget (primarily for tests; the default is generous).
    pub fn set_task_budget(&mut self, budget: Duration) {
        self.task_budget = budget;
    }

    /// Arm the runaway-script deadline for the task about to execute.
    fn arm_budget(&mut self) {
        let budget = self.task_budget;
        self.js.set_execution_deadline(Some(budget));
    }

    /// Clear the runaway-script deadline once the task (including its microtask drain) is done.
    fn disarm_budget(&mut self) {
        self.js.clear_execution_deadline();
    }

    pub fn engine_name(&self) -> &'static str {
        self.js.name()
    }

    pub fn add_module(&mut self, name: &str, source: String) {
        self.js.add_module(name, source);
    }

    pub fn memory_usage(&self) -> usize {
        self.js.memory_usage()
    }

    pub fn collect_garbage(&mut self) {
        self.js.collect_garbage();
    }

    pub fn take_errors(&mut self) -> Vec<String> {
        std::mem::take(&mut self.errors)
    }

    fn record(&mut self, context: &str, e: JsError) {
        let e = self.remap_error(e);
        let text = format!("{context}: {e}");
        self.host.borrow_mut().call_console("error", &text);
        self.errors.push(text);
    }

    /// After every task: drain microtasks, deliver mutation records, report unhandled rejections.
    fn finish_task(&mut self) {
        if let Err(e) = self.js.run_jobs() {
            self.record("microtask", e);
        }
        if let Err(e) = self.js.call_global("__rendrix_afterTask", vec![]) {
            self.record("afterTask", e);
        }
        if let Err(e) = self.js.run_jobs() {
            self.record("microtask", e);
        }
        for r in self.js.take_rejections() {
            if let Err(e) = self.js.call_global("__rendrix_unhandledRejection", vec![Val::Str(r.clone())]) {
                self.record("unhandledrejection", e);
            }
            let _ = self.js.run_jobs();
        }
        self.disarm_budget();
    }

    /// Run a classic script.
    pub fn run_script(&mut self, name: &str, source: &str) -> Result<(), JsError> {
        self.arm_budget();
        let r = self.js.eval_script(name, source);
        self.finish_task();
        r.map_err(|e| self.remap_error(e))
    }

    /// Evaluate an ES module (and its imports) registered under `name`.
    pub fn run_module(&mut self, name: &str) -> Result<(), JsError> {
        self.arm_budget();
        let r = self.js.eval_module(name);
        self.finish_task();
        r.map_err(|e| self.remap_error(e))
    }

    /// Fire DOMContentLoaded / load once all startup scripts have run.
    pub fn boot(&mut self) {
        if let Err(e) = self.js.call_global("__rendrix_boot", vec![]) {
            self.record("boot", e);
        }
        self.finish_task();
    }

    fn call(&mut self, func: &str, args: Vec<Val>) -> Val {
        self.arm_budget();
        match self.js.call_global(func, args) {
            Ok(v) => v,
            Err(e) => {
                self.record(func, e);
                Val::Undefined
            }
        }
    }

    /// Dispatch a trusted event. Returns `true` if no listener called `preventDefault`.
    pub fn dispatch(
        &mut self,
        event_type: &str,
        target: Option<NodeId>,
        init: &serde_json::Value,
    ) -> bool {
        let init_json = if init.is_null() {
            Val::Undefined
        } else {
            Val::Str(init.to_string())
        };
        let target = target.map(|t| Val::Num(t as f64)).unwrap_or(Val::Num(-1.0));
        let r = self.call(
            "__rendrix_dispatch",
            vec![Val::Str(event_type.to_string()), target, init_json],
        );
        self.finish_task();
        !matches!(r, Val::Bool(false))
    }

    /// Move focus (with blur/focus events). `None` clears focus.
    pub fn set_focus(&mut self, id: Option<NodeId>) {
        self.call(
            "__rendrix_focus",
            vec![id.map(|i| Val::Num(i as f64)).unwrap_or(Val::Null)],
        );
        self.finish_task();
    }

    /// Fire an `input` event on a control after the platform edited its value.
    pub fn fire_input(&mut self, id: NodeId, input_type: &str, data: Option<&str>) {
        self.call(
            "__rendrix_fireInput",
            vec![
                Val::Num(id as f64),
                Val::Str(input_type.into()),
                data.map(|d| Val::Str(d.into())).unwrap_or(Val::Null),
            ],
        );
        self.finish_task();
    }

    /// Fire a `change` event (e.g. when a text field loses focus after editing).
    pub fn fire_change(&mut self, id: NodeId) {
        self.dispatch("change", Some(id), &serde_json::json!({ "bubbles": true }));
    }

    /// Implicit form submission (Enter in a single-line field).
    pub fn submit_form(&mut self, id: NodeId) {
        self.call("__rendrix_submitForm", vec![Val::Num(id as f64)]);
        self.finish_task();
    }

    pub fn viewport_changed(&mut self) {
        self.call("__rendrix_viewportChanged", vec![]);
        self.finish_task();
    }

    /// Run due timers and completed fetches. Call once per event-loop iteration.
    pub fn tick(&mut self) -> TickInfo {
        let now = self.host.borrow().now_ms();
        let due = self.host.borrow_mut().take_due_timers(now);
        for id in due {
            self.call("__rendrix_timer", vec![Val::Num(id as f64)]);
            self.finish_task();
        }
        let done = self.host.borrow_mut().poll_fetches();
        for f in done {
            let mut flat = Vec::new();
            for (k, v) in &f.headers {
                flat.push(Val::Str(k.clone()));
                flat.push(Val::Str(v.clone()));
            }
            self.call(
                "__rendrix_fetchDone",
                vec![
                    Val::Num(f.id as f64),
                    Val::Num(f.status as f64),
                    Val::Str(f.status_text),
                    Val::List(flat),
                    Val::Str(base64_encode(&f.body)),
                    f.error.map(Val::Str).unwrap_or(Val::Undefined),
                ],
            );
            self.finish_task();
        }
        self.tick_info()
    }

    pub fn tick_info(&self) -> TickInfo {
        let h = self.host.borrow();
        TickInfo {
            wants_frame: h.raf_requested,
            next_timer_ms: h.next_timer_ms(),
            pending_io: h.pending_fetches > 0 || !h.immediate_fetches.is_empty(),
        }
    }

    // ------------------------------------------------------------------------------------
    // Dedicated workers (A4) pump glue. The `Worker` prelude class queues intents
    // (`create`/`post`/`terminate`) in a plain-JS outbox — no new host ops — drained here; worker
    // threads report back over an `mpsc` channel, delivered into script capped per frame.
    // ------------------------------------------------------------------------------------

    /// Snapshot of bundle modules workers resolve `new Worker(url)` against (wired from
    /// `Session::new`; in-flight workers keep the snapshot they were spawned with).
    pub fn set_worker_modules(&mut self, modules: HashMap<String, String>) {
        self.workers.set_modules(modules);
    }

    /// Whether workers may fetch `http(s)` URLs (mirrors the bundle `network` permission).
    pub fn set_worker_network(&mut self, allow: bool) {
        self.workers.set_network(allow);
    }

    /// Pump worker threads once: drain the main→worker outbox, then deliver up to
    /// [`WORKER_MAX_PER_TICK`] worker→main events into script. Never blocks. Returns the
    /// delivered events so the frame driver can schedule a wake while more are queued.
    pub fn poll_workers(&mut self) -> Vec<WorkerEvent> {
        // 1. Main→worker intents queued by the `Worker` prelude class: [kind, id, a, b].
        let outbox: Vec<(String, u32, String, String)> =
            match self.js.call_global("__rendrix_workerTakeOutbox", vec![]) {
                Ok(Val::List(items)) => items
                    .into_iter()
                    .filter_map(|v| match v {
                        Val::List(f) => {
                            let mut it = f.into_iter();
                            let kind = it.next()?.as_str()?.to_string();
                            let id = it.next()?.as_f64()?;
                            let a = it
                                .next()
                                .and_then(|v| v.as_str().map(str::to_string))
                                .unwrap_or_default();
                            let b = it
                                .next()
                                .and_then(|v| v.as_str().map(str::to_string))
                                .unwrap_or_default();
                            (id >= 0.0).then(|| (kind, id as u32, a, b))
                        }
                        _ => None,
                    })
                    .collect(),
                Ok(_) => Vec::new(),
                Err(e) => {
                    self.record("workerOutbox", e);
                    Vec::new()
                }
            };
        self.finish_task();
        for (kind, id, a, b) in outbox {
            match kind.as_str() {
                "create" => self.workers.spawn(id, &a, &b),
                "post" => self.workers.post(id, &a),
                "terminate" => self.workers.terminate(id),
                _ => {}
            }
        }
        // 2. Worker→main events, capped per frame (the spill stays queued for the next frame).
        let (events, _) = self.workers.drain_capped(WORKER_MAX_PER_TICK);
        for ev in &events {
            self.deliver_worker_event(ev);
        }
        self.finish_task();
        events
    }

    fn deliver_worker_event(&mut self, ev: &WorkerEvent) {
        match ev {
            WorkerEvent::Message { worker, json } => {
                if let Err(e) = self.js.call_global(
                    "__rendrix_workerDeliver",
                    vec![Val::Num(*worker as f64), Val::Str(json.clone())],
                ) {
                    self.record("workerMessage", e);
                }
            }
            WorkerEvent::Error { worker, message } => {
                if let Err(e) = self.js.call_global(
                    "__rendrix_workerError",
                    vec![Val::Num(*worker as f64), Val::Str(message.clone())],
                ) {
                    self.record("workerError", e);
                }
            }
            WorkerEvent::Closed { worker } => {
                if let Err(e) = self
                    .js
                    .call_global("__rendrix_workerClosed", vec![Val::Num(*worker as f64)])
                {
                    self.record("workerClosed", e);
                }
                self.workers.note_closed(*worker);
            }
            WorkerEvent::Console {
                worker,
                level,
                text,
            } => {
                self.host
                    .borrow_mut()
                    .call_console(level, &format!("[worker {worker}] {text}"));
            }
        }
    }

    /// True when a capped drain left worker events queued (the frame driver should wake soon).
    pub fn worker_pending(&self) -> bool {
        self.workers.pending_delivery()
    }

    /// True when any worker slot is still alive (drives platform wake cadence while workers run).
    pub fn worker_alive(&self) -> bool {
        self.workers.alive()
    }

    /// Prompt exit of every worker, waiting only a bounded budget (never deadlocks shutdown).
    pub fn shutdown_workers(&mut self) {
        self.workers.shutdown();
    }

    /// Run `requestAnimationFrame` callbacks for this frame (timestamp in ms).
    pub fn run_animation_frame(&mut self, timestamp_ms: f64) {
        self.host.borrow_mut().raf_requested = false;
        let more = self.call("__rendrix_raf", vec![Val::Num(timestamp_ms)]);
        if matches!(more, Val::Bool(true)) {
            self.host.borrow_mut().raf_requested = true;
        }
        self.finish_task();
    }

    /// Notify Resize/IntersectionObservers after layout (runs inside the frame, after `Engine::update`).
    /// A single JS entry point (`checkObservers`) fans out to both observer kinds so an
    /// idle frame costs one cheap call that early-returns when no observers exist.
    pub fn after_layout(&mut self) {
        self.call("__rendrix_checkObservers", vec![]);
        self.finish_task();
    }

    pub fn take_window_commands(&mut self) -> Vec<WindowCommand> {
        std::mem::take(&mut self.host.borrow_mut().window_commands)
    }

    pub fn quit_requested(&self) -> bool {
        self.host.borrow().quit_requested
    }

    /// Evaluate an expression and return its value (primitives and arrays of them), for tests and diagnostics.
    pub fn eval(&mut self, expr: &str) -> Result<Val, JsError> {
        self.arm_budget();
        let src = format!("globalThis.__rendrix_eval_get = function(){{ return ({expr}); }};");
        let r = self
            .js
            .eval_script("eval", &src)
            .and_then(|_| self.js.call_global("__rendrix_eval_get", vec![]));
        self.finish_task();
        r.map_err(|e| self.remap_error(e))
    }
}

impl DomHost {
    pub fn call_console(&mut self, level: &str, msg: &str) {
        use crate::engine_trait::Host;
        let _ = self.call("console", &[Val::Str(level.into()), Val::Str(msg.into())]);
    }
}
