//! Dedicated workers (A4): one isolated QuickJS context per worker on its own OS thread,
//! bridged into the main-thread `Session` pump without ever blocking the frame loop.
//!
//! Design (all constraints from the task brief):
//! - QuickJS is retained (`rquickjs`, same as the main context). No V8, no new engine.
//! - No new native dependencies: `std::thread` + `std::sync::mpsc` only.
//! - Workers are isolated: they run a subset prelude (timers, fetch, console,
//!   `structuredClone`, `postMessage`/`onmessage`) with **no DOM access** — `document`, `window`
//!   and friends are throwing accessors. There is no `SharedArrayBuffer` in v1: message transport
//!   is JSON + base64-tagged binary (structured-clone semantics, cloned — a `transfer` list is
//!   accepted syntactically and ignored semantically).
//! - The pump never waits on a worker thread: all crossings are unbounded `mpsc` channels drained
//!   with `try_recv` / bounded `recv_timeout`. Delivery into the main script is capped at
//!   [`WORKER_MAX_PER_TICK`] events per frame (same spill pattern as the WebSocket 16/frame cap);
//!   the remainder stays queued in the channel for the next frame.
//! - The main-thread `Worker` class talks to this registry through a plain-JS outbox drained by
//!   `JsRuntime::poll_workers` — no new `host.rs` ops were needed.
//!
//! Ownership (for the main agent wiring `session.rs` / `window.rs`):
//! - `WorkerRegistry` is owned by `JsRuntime` (main thread only; `QuickJsEngine` is `!Send` and
//!   never crosses threads). Each worker thread owns its `QuickJsEngine` + `WorkerHost` outright.
//! - `Session::frame` calls `poll_workers()` (drains outbox, pumps threads, delivers capped events
//!   into script) and schedules a wake when `worker_pending()` reports spilled events. `Session`
//!   shutdown calls `shutdown_workers()` (bounded join, then detach — never deadlocks).

use crate::engine_trait::{Host, JsEngine, Val};
use crate::quickjs::{resolve_module, QuickJsEngine};
use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap, VecDeque};
use std::rc::Rc;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::time::{Duration, Instant};

/// Maximum worker→main events delivered to script per frame; the rest stays queued for the next
/// frame (mirrors the WebSocket `WS_MAX_PER_TICK` spill pattern).
pub const WORKER_MAX_PER_TICK: usize = 16;
/// Per-task execution budget inside a worker (same value as the main context's `DEFAULT_TASK_BUDGET`).
const WORKER_TASK_BUDGET: Duration = Duration::from_secs(5);
/// How long the worker thread parks waiting for the next command before pumping timers/fetches.
const WORKER_PARK: Duration = Duration::from_millis(8);
/// Bounded shutdown wait; threads that ignore `Terminate` past this are detached, never joined
/// forever (a spinning worker thread can never stall shutdown or the frame loop).
const SHUTDOWN_BUDGET: Duration = Duration::from_millis(500);

const EVENTS_JS: &str = include_str!("prelude/events.js");

/// Subset prelude for worker threads. Everything a worker script can touch is installed on the
/// global object here; `__native` is captured in the closure and never exposed. DOM-adjacent
/// globals throw on access. `N('worker_post' | 'worker_error' | 'worker_close')` are the only
/// bridge ops, served by [`WorkerHost`] on the same thread.
const WORKER_JS: &str = r#"
const windowTarget = new EventTarget();

// --- time ------------------------------------------------------------------
global.performance = { now: () => N('now'), timeOrigin: Date.now(), toJSON() { return { timeOrigin: this.timeOrigin }; } };

// --- console (forwarded to the main console as `[worker N] ...`) ------------
function __wFmt(a) { try { return typeof a === 'string' ? a : JSON.stringify(a); } catch (_) { try { return String(a); } catch (_2) { return '[unprintable]'; } } }
global.console = { log: (...a) => N('console', 'log', a.map(__wFmt).join(' ')), info: (...a) => N('console', 'info', a.map(__wFmt).join(' ')),
  debug: (...a) => N('console', 'debug', a.map(__wFmt).join(' ')), warn: (...a) => N('console', 'warn', a.map(__wFmt).join(' ')),
  error: (...a) => N('console', 'error', a.map(__wFmt).join(' ')), trace: (...a) => N('console', 'trace', a.map(__wFmt).join(' ')),
  assert: (c, ...a) => { if (!c) N('console', 'error', 'Assertion failed' + (a.length ? ': ' + a.map(__wFmt).join(' ') : '')); },
  clear: () => {}, count: () => {}, countReset: () => {}, group: () => {}, groupCollapsed: () => {}, groupEnd: () => {},
  time: () => {}, timeEnd: () => {}, timeLog: () => {}, table: (d) => N('console', 'log', __wFmt(d)), dir: (d) => N('console', 'log', __wFmt(d)), dirxml: (d) => N('console', 'log', __wFmt(d)) };

// --- timers ------------------------------------------------------------------
let __wTimerSeq = 1; const __wTimerCbs = new Map();
function __wAddTimer(fn, delay, args, repeat) {
  if (typeof fn !== 'function') throw new TypeError('Timer callback must be a function');
  const id = __wTimerSeq++;
  __wTimerCbs.set(id, { fn, args, repeat });
  N('timer_set', id, Math.max(0, +delay || 0), repeat);
  return id;
}
global.setTimeout = (fn, delay, ...args) => __wAddTimer(fn, delay, args, false);
global.setInterval = (fn, delay, ...args) => __wAddTimer(fn, delay, args, true);
global.clearTimeout = id => { if (__wTimerCbs.delete(id)) N('timer_clear', id); };
global.clearInterval = global.clearTimeout;
global.setImmediate = (fn, ...args) => __wAddTimer(fn, 0, args, false);
global.clearImmediate = global.clearTimeout;
global.queueMicrotask = fn => { Promise.resolve().then(() => { try { fn(); } catch (e) { reportError(e); } }); };
global.__rendrix_workerTimer = id => {
  const t = __wTimerCbs.get(id); if (!t) return;
  if (!t.repeat) __wTimerCbs.delete(id);
  try { t.fn(...t.args); } catch (e) { reportError(e); }
};

// --- base64 ------------------------------------------------------------------
const __wB64 = 'ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/';
function __wBytesToB64(b) {
  let s = '';
  for (let i = 0; i < b.length; i += 3) {
    const n = b[i] << 16 | (b[i + 1] || 0) << 8 | (b[i + 2] || 0);
    s += __wB64[n >> 18 & 63] + __wB64[n >> 12 & 63] + (i + 1 < b.length ? __wB64[n >> 6 & 63] : '=') + (i + 2 < b.length ? __wB64[n & 63] : '=');
  }
  return s;
}
function __wB64ToBytes(s) {
  s = String(s).replace(/[\s=]/g, '').replace(/-/g, '+').replace(/_/g, '/');
  const out = []; let acc = 0, bits = 0;
  for (const ch of s) { const v = __wB64.indexOf(ch); if (v < 0) throw new Error('invalid base64'); acc = acc << 6 | v; bits += 6; if (bits >= 8) { bits -= 8; out.push(acc >> bits & 255); acc &= (1 << bits) - 1; } }
  return new Uint8Array(out);
}

// --- structured-clone transport codec (JSON + tagged binary; KEEP IN SYNC with web.js) ---
// The encoder *is* the clone: the receiver always gets a fresh value. `transfer` lists are
// accepted by `postMessage` and ignored (cloned, not neutered). No SharedArrayBuffer in v1.
function __workerEncode(v) {
  const seen = new Map();
  function enc(x) {
    if (x === undefined || typeof x === 'function' || typeof x === 'symbol') throw new DOMException('The object could not be cloned.', 'DataCloneError');
    if (x === null || typeof x !== 'object') {
      if (typeof x === 'number') { if (Number.isNaN(x)) return { $t: 'num', v: 'NaN' }; if (x === Infinity) return { $t: 'num', v: 'Inf' }; if (x === -Infinity) return { $t: 'num', v: '-Inf' }; return x; }
      if (typeof x === 'bigint') return { $t: 'big', v: x.toString() };
      return x;
    }
    // Value types clone by value and are never memoised: only containers (Map/Set/Array/plain
    // objects) occupy memo/table slots, so `$ref` indices agree with the decoder.
    if (x instanceof Date) return { $t: 'date', v: x.getTime() };
    if (x instanceof RegExp) return { $t: 're', v: x.source, f: x.flags };
    if (x instanceof ArrayBuffer) return { $t: 'bytes', k: 'u8', v: __wBytesToB64(new Uint8Array(x)) };
    if (ArrayBuffer.isView(x)) {
      const k = x instanceof Float64Array ? 'f64' : x instanceof Float32Array ? 'f32' : x instanceof Int32Array ? 'i32' : x instanceof Uint32Array ? 'u32' : x instanceof Int16Array ? 'i16' : x instanceof Uint16Array ? 'u16' : x instanceof Int8Array ? 'i8' : 'u8';
      return { $t: 'bytes', k, v: __wBytesToB64(new Uint8Array(x.buffer, x.byteOffset, x.byteLength)) };
    }
    if (x instanceof Error) return { $t: 'err', v: x.name || 'Error', m: x.message || '' };
    if (seen.has(x)) return { $t: 'ref', v: seen.get(x) };
    seen.set(x, seen.size);
    if (x instanceof Map) return { $t: 'map', v: [...x].map(([k, y]) => [enc(k), enc(y)]) };
    if (x instanceof Set) return { $t: 'set', v: [...x].map(enc) };
    if (Array.isArray(x)) return x.map(enc);
    const out = {};
    for (const k of Object.keys(x)) out[k] = enc(x[k]);
    return out;
  }
  return JSON.stringify(enc(v));
}
function __workerDecode(json) {
  const table = [];
  function dec(x) {
    if (x === null || typeof x !== 'object') return x;
    if (Array.isArray(x)) { const o = []; table.push(o); for (const e of x) o.push(dec(e)); return o; }
    if (!('$t' in x)) { const o = {}; table.push(o); for (const k of Object.keys(x)) o[k] = dec(x[k]); return o; }
    switch (x.$t) {
      case 'ref': return table[x.v];
      case 'date': return new Date(x.v);
      case 're': return new RegExp(x.v, x.f || '');
      case 'map': { const m = new Map(); table.push(m); for (const [k, y] of x.v) m.set(dec(k), dec(y)); return m; }
      case 'set': { const st = new Set(); table.push(st); for (const y of x.v) st.add(dec(y)); return st; }
      case 'bytes': {
        const b = __wB64ToBytes(x.v);
        switch (x.k) {
          case 'f64': return new Float64Array(b.buffer); case 'f32': return new Float32Array(b.buffer);
          case 'i32': return new Int32Array(b.buffer); case 'u32': return new Uint32Array(b.buffer);
          case 'i16': return new Int16Array(b.buffer); case 'u16': return new Uint16Array(b.buffer);
          case 'i8': return new Int8Array(b.buffer); default: return new Uint8Array(b.buffer);
        }
      }
      case 'err': { const e = new Error(x.m); e.name = x.v; return e; }
      case 'num': return x.v === 'NaN' ? NaN : x.v === 'Inf' ? Infinity : -Infinity;
      case 'big': return BigInt(x.v);
      default: return undefined;
    }
  }
  return dec(JSON.parse(json));
}
global.structuredClone = v => __workerDecode(__workerEncode(v));

// --- fetch (data:/http(s); bundle-relative URLs are a documented v1 gap) ----
let __wFetchSeq = 1; const __wPendingFetches = new Map();
class __wHeaders {
  constructor() { this._m = new Map(); }
  get(k) { const v = this._m.get(String(k).toLowerCase()); return v === undefined ? null : v; }
  has(k) { return this._m.has(String(k).toLowerCase()); }
  forEach(fn, t) { for (const [k, v] of this._m) fn.call(t, v, k, this); }
}
class __wResponse {
  constructor(bytes, init) {
    this._bytes = bytes; this.status = init.status; this.statusText = init.statusText || '';
    this.headers = new __wHeaders(); for (const k of Object.keys(init.headers || {})) this.headers._m.set(k.toLowerCase(), init.headers[k]);
    this.url = init.url || ''; this.type = 'basic'; this.redirected = false; this.bodyUsed = false;
  }
  get ok() { return this.status >= 200 && this.status < 300; }
  text() { if (this.bodyUsed) return Promise.reject(new TypeError('Body has already been consumed')); this.bodyUsed = true; let s = ''; for (const x of this._bytes) s += String.fromCharCode(x); return Promise.resolve(s); }
  json() { return this.text().then(JSON.parse); }
  arrayBuffer() { if (this.bodyUsed) return Promise.reject(new TypeError('Body has already been consumed')); this.bodyUsed = true; return Promise.resolve(this._bytes.buffer.slice(0)); }
}
global.fetch = (input, init = {}) => new Promise((resolve, reject) => {
  const url = typeof input === 'string' ? input : String((input && input.url) || input);
  const method = String((init && init.method) || 'GET').toUpperCase();
  const flat = [];
  if (init && init.headers) { if (Array.isArray(init.headers)) for (const [k, v] of init.headers) flat.push(String(k), String(v)); else for (const k of Object.keys(init.headers)) flat.push(k, String(init.headers[k])); }
  let bodyB64 = null;
  if (init && init.body !== undefined && init.body !== null) {
    const b = typeof init.body === 'string' ? (() => { const o = []; for (let i = 0; i < init.body.length; i++) o.push(init.body.charCodeAt(i) & 255); return new Uint8Array(o); })()
      : init.body instanceof Uint8Array ? init.body : new Uint8Array(init.body);
    bodyB64 = __wBytesToB64(b);
  }
  const id = __wFetchSeq++;
  __wPendingFetches.set(id, { resolve, reject, url });
  try { N('fetch_start', id, url, method, flat, bodyB64); }
  catch (e) { __wPendingFetches.delete(id); reject(e); }
});
global.__rendrix_workerFetchDone = (id, status, statusText, headersFlat, bodyB64, err) => {
  const f = __wPendingFetches.get(id); if (!f) return; __wPendingFetches.delete(id);
  if (err) { f.reject(new Error('Failed to fetch: ' + err)); return; }
  const h = {}; for (let i = 0; i < headersFlat.length; i += 2) h[headersFlat[i]] = headersFlat[i + 1];
  f.resolve(new __wResponse(__wB64ToBytes(bodyB64 || ''), { status, statusText, headers: h, url: f.url }));
};

// --- worker scope: postMessage / onmessage / close ---------------------------
global.postMessage = (data, transfer) => { N('worker_post', __workerEncode(data)); };
global.close = () => { try { N('worker_close'); } catch (_) {} };
global.self = global;
const __wScope = new EventTarget();
for (const m of ['addEventListener', 'removeEventListener', 'dispatchEvent']) global[m] = (...a) => __wScope[m](...a);
for (const t of ['message', 'messageerror', 'error']) {
  Object.defineProperty(global, 'on' + t, {
    configurable: true,
    get() { return __wScope['on' + t]; },
    set(v) { __wScope['on' + t] = v; },
  });
}
global.__rendrix_workerDispatch = json => {
  let data;
  try { data = __workerDecode(json); }
  catch (_) { global.dispatchEvent(new MessageEvent('messageerror', {})); return; }
  global.dispatchEvent(new MessageEvent('message', { data }));
};
global.EventTarget = EventTarget; global.Event = Event; global.MessageEvent = MessageEvent; global.ErrorEvent = ErrorEvent; global.DOMException = DOMException;

// --- isolation: DOM and main-thread-only APIs throw on access -----------------
function __wDeny(name, what) {
  Object.defineProperty(global, name, {
    configurable: true, enumerable: false,
    get() { throw new Error(name + ' is not available in dedicated workers' + (what ? ' (' + what + ')' : '') + ' (no DOM access)'); },
    set(_) { throw new Error(name + ' is not available in dedicated workers (no DOM access)'); },
  });
}
for (const k of ['document', 'window', 'location', 'navigator', 'localStorage', 'sessionStorage',
  'alert', 'confirm', 'prompt', 'open', 'getComputedStyle', 'matchMedia', 'requestAnimationFrame',
  'cancelAnimationFrame', 'XMLHttpRequest', 'WebSocket', 'ResizeObserver', 'IntersectionObserver',
  'MutationObserver', 'customElements', 'Image', 'importScripts']) __wDeny(k);
// Nested workers are a v1 gap: fail loudly instead of half-working.
global.Worker = class Worker { constructor() { throw new DOMException('Nested workers are not supported in this release', 'NotSupportedError'); } };

// Uncaught worker errors (listener throws, etc.) surface as `error` events on the main `Worker`
// *and* stay in the worker console. `reportError` is shared with events.js (same scope).
reportError = ((orig) => (err) => { try { N('worker_error', String((err && err.message) || err)); } catch (_) {} return orig(err); })(reportError);
"#;

/// Commands from the main thread to one worker thread. All crossings are non-blocking sends.
enum WorkerCommand {
    Post(String),
    Terminate,
}

/// Events from a worker thread to the main thread, delivered into script by `poll_workers`.
#[derive(Debug, Clone)]
pub enum WorkerEvent {
    Message {
        worker: u32,
        json: String,
    },
    Error {
        worker: u32,
        message: String,
    },
    Console {
        worker: u32,
        level: String,
        text: String,
    },
    Closed {
        worker: u32,
    },
}

impl WorkerEvent {
    /// Worker id this event belongs to.
    pub fn worker(&self) -> u32 {
        match self {
            WorkerEvent::Message { worker, .. }
            | WorkerEvent::Error { worker, .. }
            | WorkerEvent::Console { worker, .. }
            | WorkerEvent::Closed { worker } => *worker,
        }
    }
}

struct WorkerSlot {
    cmd: Sender<WorkerCommand>,
    handle: Option<std::thread::JoinHandle<()>>,
    terminated: bool,
}

/// Main-thread registry: owns worker threads, their module snapshots and the event channel.
/// Never blocks: sends are unbounded-`mpsc` (never wait), receives are `try_recv`.
pub struct WorkerRegistry {
    modules: HashMap<String, String>,
    network: bool,
    slots: HashMap<u32, WorkerSlot>,
    tx: Sender<WorkerEvent>,
    rx: Receiver<WorkerEvent>,
    /// Events pulled past the per-frame cap; drained first next frame.
    stash: VecDeque<WorkerEvent>,
    /// Set when a drain hit the cap with more queued (drives `Session` wake scheduling).
    overflow: bool,
}

impl WorkerRegistry {
    pub fn new() -> WorkerRegistry {
        let (tx, rx) = channel();
        WorkerRegistry {
            modules: HashMap::new(),
            network: false,
            slots: HashMap::new(),
            tx,
            rx,
            stash: VecDeque::new(),
            overflow: false,
        }
    }

    /// Snapshot of bundle modules workers resolve `new Worker(url)` against. Called from
    /// `Session::new` (main-agent wiring); cheap enough to refresh any time — in-flight workers
    /// keep the snapshot they were spawned with.
    pub fn set_modules(&mut self, modules: HashMap<String, String>) {
        self.modules = modules;
    }

    /// Whether workers may fetch `http(s)` URLs (mirrors the bundle `network` permission).
    pub fn set_network(&mut self, allow: bool) {
        self.network = allow;
    }

    /// Spawn (or re-spawn) the worker for main-side `id`. `url` is the raw `Worker()` argument,
    /// `kind` is `"module"` or `"classic"`. Failures (unknown module, thread spawn) surface as
    /// `Error` + `Closed` events so the main `Worker` object always settles.
    pub fn spawn(&mut self, id: u32, url: &str, kind: &str) {
        if let Some(old) = self.slots.remove(&id) {
            let _ = old.cmd.send(WorkerCommand::Terminate);
            // The old thread reaps itself via its `Closed` event; a live handle that outlives the
            // slot is joined in `reap_finished` / `shutdown`.
            if let Some(h) = old.handle {
                if h.is_finished() {
                    let _ = h.join();
                } else {
                    self.slots.insert(
                        id,
                        WorkerSlot {
                            cmd: old.cmd,
                            handle: Some(h),
                            terminated: true,
                        },
                    );
                }
            }
            if !self.slots.contains_key(&id) {
                // Old slot fully gone; fall through to fresh spawn below.
            } else {
                return; // Old thread still exiting; keep its slot (terminated) until `Closed`.
            }
        }
        let fail = |tx: &Sender<WorkerEvent>, message: String| {
            let _ = tx.send(WorkerEvent::Error {
                worker: id,
                message,
            });
            let _ = tx.send(WorkerEvent::Closed { worker: id });
        };
        let (key, source) = match resolve_worker_key(url, &self.modules) {
            Some(ok) => ok,
            None => {
                fail(&self.tx, format!("Worker module not found for '{url}' (bundle-relative path, e.g. new Worker('worker.js'))"));
                return;
            }
        };
        let (cmd_tx, cmd_rx) = channel();
        let tx = self.tx.clone();
        let modules = self.modules.clone();
        let network = self.network;
        let is_module = kind == "module";
        let spec = url.to_string();
        match std::thread::Builder::new()
            .name(format!("rendrix-worker-{id}"))
            .spawn(move || {
                worker_main(
                    id, key, source, is_module, spec, modules, network, tx, cmd_rx,
                );
            }) {
            Ok(handle) => {
                self.slots.insert(
                    id,
                    WorkerSlot {
                        cmd: cmd_tx,
                        handle: Some(handle),
                        terminated: false,
                    },
                );
            }
            Err(e) => fail(&self.tx, format!("cannot spawn worker thread: {e}")),
        }
    }

    /// Queue a message for a worker. Unknown or terminated workers ignore it (no crash — matches
    /// the `terminate()-then-post` requirement).
    pub fn post(&mut self, id: u32, json: &str) {
        if let Some(slot) = self.slots.get(&id) {
            if !slot.terminated {
                let _ = slot.cmd.send(WorkerCommand::Post(json.to_string()));
            }
        }
    }

    /// Ask a worker to stop. The thread exits on its own; the slot is reaped when its `Closed`
    /// event arrives (or in `shutdown`). Never waits.
    pub fn terminate(&mut self, id: u32) {
        if let Some(slot) = self.slots.get_mut(&id) {
            slot.terminated = true;
            let _ = slot.cmd.send(WorkerCommand::Terminate);
        }
    }

    /// Reap threads that already finished (covers panics, which never send `Closed`): join them
    /// without blocking and synthesise the `Closed` event so the main `Worker` object settles.
    fn reap_finished(&mut self) {
        let mut closed = Vec::new();
        for (id, slot) in self.slots.iter_mut() {
            if let Some(h) = slot.handle.take() {
                if h.is_finished() {
                    let _ = h.join();
                    closed.push(*id);
                } else {
                    slot.handle = Some(h);
                }
            }
        }
        for id in closed {
            self.slots.remove(&id);
            self.stash.push_back(WorkerEvent::Closed { worker: id });
        }
    }

    /// Note a delivered `Closed` event: drop the slot (joining the already-exited thread) so ids
    /// and OS threads are recycled — no leak on rapid create/terminate.
    pub fn note_closed(&mut self, id: u32) {
        if let Some(slot) = self.slots.remove(&id) {
            if let Some(h) = slot.handle {
                // The thread sent `Closed` as its last act, so it has exited; `join` returns
                // immediately. If it somehow hasn't, detach rather than stall the frame loop.
                if h.is_finished() {
                    let _ = h.join();
                }
            }
        }
        self.overflow = !self.stash.is_empty();
    }

    /// Drain worker events, at most `cap` (the rest stays queued). Returns the events plus whether
    /// more are waiting (drives `Session` wake scheduling).
    pub fn drain_capped(&mut self, cap: usize) -> (Vec<WorkerEvent>, bool) {
        self.reap_finished();
        let mut out = Vec::new();
        while out.len() < cap {
            if let Some(ev) = self.stash.pop_front() {
                out.push(ev);
                continue;
            }
            match self.rx.try_recv() {
                Ok(ev) => out.push(ev),
                Err(_) => break,
            }
        }
        // One extra probe: did we stop at the cap with more waiting?
        if self.stash.is_empty() {
            if let Ok(ev) = self.rx.try_recv() {
                self.stash.push_back(ev);
            }
        }
        let more = !self.stash.is_empty();
        self.overflow = more;
        (out, more)
    }

    pub fn pending_delivery(&self) -> bool {
        self.overflow
    }

    /// Any worker slot not yet reaped (drives platform wake cadence while workers are alive).
    pub fn alive(&self) -> bool {
        !self.slots.is_empty()
    }

    /// Prompt exit of every worker, waiting only a bounded budget; threads that ignore `Terminate`
    /// are detached (they exit on their own via the runaway budget / channel disconnect). Never
    /// deadlocks: no blocking join, no frame-loop involvement.
    pub fn shutdown(&mut self) {
        for slot in self.slots.values() {
            let _ = slot.cmd.send(WorkerCommand::Terminate);
        }
        let deadline = Instant::now() + SHUTDOWN_BUDGET;
        loop {
            for slot in self.slots.values_mut() {
                if let Some(h) = slot.handle.take() {
                    if h.is_finished() {
                        let _ = h.join();
                    } else {
                        slot.handle = Some(h);
                    }
                }
            }
            self.slots.retain(|_, s| s.handle.is_some());
            // Drain any final events so `Closed` bookkeeping stays consistent for a later reuse.
            while let Ok(ev) = self.rx.try_recv() {
                if let WorkerEvent::Closed { worker } = ev {
                    self.slots.remove(&worker);
                }
            }
            if self.slots.is_empty() || Instant::now() >= deadline {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        // Anything left is detached (its sender dies with the slot); it exits on its own.
        self.slots.clear();
        self.overflow = false;
    }
}

impl Default for WorkerRegistry {
    fn default() -> Self {
        WorkerRegistry::new()
    }
}

/// Bundle-relative module resolution for `new Worker(url)`, reusing the `MapLoader` semantics:
/// `rendrix://app/` prefixes, query/hash, leading `/` and `./` are stripped, then the same
/// `./`-relative + `.js`/`/index.js`/`.mjs` probing as normal module imports applies.
///
/// Caveats (documented, v1): the URL must name a bundle module — bare npm-style specifiers work
/// only if they literally match a bundle key (the CLI build rewrites *static* `import`s, but a
/// `Worker()` URL string is opaque to the bundler). Dynamic `import()` *inside* a worker resolves
/// through the same map against the worker's own URL, like a normal module graph.
fn resolve_worker_key(url: &str, modules: &HashMap<String, String>) -> Option<(String, String)> {
    let mut spec = url.trim().to_string();
    if let Some(rest) = spec.strip_prefix("rendrix://app/") {
        spec = rest.to_string();
    }
    spec = spec.split(['?', '#']).next().unwrap_or("").to_string();
    spec = spec.trim_start_matches('/').to_string();
    if spec.is_empty() {
        return None;
    }
    let key = resolve_module(modules, "", &spec)?;
    let source = modules.get(&key)?.clone();
    Some((key, source))
}

// ------------------------------------------------------------------------------------
// Worker thread
// ------------------------------------------------------------------------------------

struct WorkerFetch {
    id: u32,
    status: u16,
    status_text: String,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
    error: Option<String>,
}

/// Minimal native host for a worker thread. Owns everything it touches (timers, console, fetch
/// channels) — nothing is shared with the main `DomHost`, so no locking can stall the frame loop.
/// Any DOM op is simply unknown here (the worker prelude never emits one); it fails loudly.
struct WorkerHost {
    timers: BTreeMap<u32, (f64, Option<f64>)>,
    console: Vec<(String, String)>,
    start: Instant,
    network: bool,
    fetch_tx: Sender<WorkerFetch>,
    fetch_rx: Receiver<WorkerFetch>,
    immediate: Vec<WorkerFetch>,
    closed: bool,
    worker_id: u32,
    event_tx: Sender<WorkerEvent>,
}

impl WorkerHost {
    fn new(worker_id: u32, network: bool, event_tx: Sender<WorkerEvent>) -> WorkerHost {
        let (fetch_tx, fetch_rx) = channel();
        WorkerHost {
            timers: BTreeMap::new(),
            console: Vec::new(),
            start: Instant::now(),
            network,
            fetch_tx,
            fetch_rx,
            immediate: Vec::new(),
            closed: false,
            worker_id,
            event_tx,
        }
    }

    fn now_ms(&self) -> f64 {
        self.start.elapsed().as_secs_f64() * 1000.0
    }

    fn take_due_timers(&mut self, now_ms: f64) -> Vec<u32> {
        let due: Vec<u32> = self
            .timers
            .iter()
            .filter(|(_, (t, _))| *t <= now_ms)
            .map(|(i, _)| *i)
            .collect();
        for i in &due {
            if let Some((t, interval)) = self.timers.get(i).copied() {
                match interval {
                    Some(iv) => {
                        self.timers
                            .insert(*i, ((t + iv).max(now_ms + 1.0), Some(iv)));
                    }
                    None => {
                        self.timers.remove(i);
                    }
                }
            }
        }
        due
    }

    fn take_console(&mut self) -> Vec<(String, String)> {
        std::mem::take(&mut self.console)
    }

    fn poll_fetches(&mut self) -> Vec<WorkerFetch> {
        let mut out = std::mem::take(&mut self.immediate);
        while let Ok(r) = self.fetch_rx.try_recv() {
            out.push(r);
        }
        out
    }

    fn rand(&mut self) -> f64 {
        let mut buf = [0u8; 8];
        if getrandom::getrandom(&mut buf).is_err() {
            panic!("the operating system random number generator is unavailable");
        }
        (u64::from_le_bytes(buf) >> 11) as f64 / (1u64 << 53) as f64
    }

    fn start_fetch(
        &mut self,
        fid: u32,
        url: String,
        method: String,
        headers: Vec<(String, String)>,
        body: Option<Vec<u8>>,
    ) {
        let done = |id, status: u16, text: &str, headers: Vec<(String, String)>, body: Vec<u8>| {
            WorkerFetch {
                id,
                status,
                status_text: text.to_string(),
                headers,
                body,
                error: None,
            }
        };
        let fail = |id, msg: String| WorkerFetch {
            id,
            status: 0,
            status_text: String::new(),
            headers: vec![],
            body: vec![],
            error: Some(msg),
        };
        if let Some(rest) = url.strip_prefix("data:") {
            let r = match rest.split_once(',') {
                Some((meta, data)) => {
                    let bytes = if meta.ends_with(";base64") {
                        crate::host::base64_decode(data)
                    } else {
                        Some(worker_percent_decode(data))
                    };
                    match bytes {
                        Some(b) => done(
                            fid,
                            200,
                            "OK",
                            vec![(
                                "content-type".into(),
                                meta.trim_end_matches(";base64").to_string(),
                            )],
                            b,
                        ),
                        None => fail(fid, "invalid data: URL".into()),
                    }
                }
                None => fail(fid, "invalid data: URL".into()),
            };
            self.immediate.push(r);
            return;
        }
        if !url.contains("://") {
            // v1 gap: no asset bytes are cloned into the worker (documented). Dynamic `import()`
            // of bundle modules *does* work; only `fetch()` of relative URLs is unsupported.
            self.immediate.push(fail(fid, "worker fetch of bundle-relative URLs is not supported in v1 (use http(s) or data: URLs)".into()));
            return;
        }
        if !self.network {
            self.immediate.push(fail(
                fid,
                "network access is not permitted for this app".into(),
            ));
            return;
        }
        let tx = self.fetch_tx.clone();
        std::thread::spawn(move || {
            let mut req = ureq::request(&method, &url).timeout(std::time::Duration::from_secs(60));
            for (k, v) in &headers {
                req = req.set(k, v);
            }
            let resp = match body {
                Some(b) => req.send_bytes(&b),
                None => req.call(),
            };
            let result = match resp {
                Ok(r) | Err(ureq::Error::Status(_, r)) => {
                    let status = r.status();
                    let text = r.status_text().to_string();
                    let hdrs: Vec<(String, String)> = r
                        .headers_names()
                        .into_iter()
                        .filter_map(|n| r.header(&n).map(|v| (n, v.to_string())))
                        .collect();
                    let mut buf = Vec::new();
                    use std::io::Read;
                    match r.into_reader().take(64 * 1024 * 1024).read_to_end(&mut buf) {
                        Ok(_) => WorkerFetch {
                            id: fid,
                            status,
                            status_text: text,
                            headers: hdrs,
                            body: buf,
                            error: None,
                        },
                        Err(e) => WorkerFetch {
                            id: fid,
                            status: 0,
                            status_text: String::new(),
                            headers: vec![],
                            body: vec![],
                            error: Some(e.to_string()),
                        },
                    }
                }
                Err(e) => WorkerFetch {
                    id: fid,
                    status: 0,
                    status_text: String::new(),
                    headers: vec![],
                    body: vec![],
                    error: Some(e.to_string()),
                },
            };
            let _ = tx.send(result);
        });
    }
}

fn worker_percent_decode(s: &str) -> Vec<u8> {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let Some(v) = std::str::from_utf8(&b[i + 1..i + 3])
                .ok()
                .and_then(|h| u8::from_str_radix(h, 16).ok())
            {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    out
}

fn ws_arg(args: &[Val], i: usize) -> String {
    match args.get(i) {
        Some(Val::Str(s)) => s.clone(),
        Some(Val::Num(n)) => n.to_string(),
        Some(Val::Bool(b)) => b.to_string(),
        _ => String::new(),
    }
}

fn wnum(args: &[Val], i: usize) -> f64 {
    args.get(i).and_then(|v| v.as_f64()).unwrap_or(0.0)
}

impl Host for WorkerHost {
    fn call(&mut self, op: &str, a: &[Val]) -> Result<Val, String> {
        Ok(match op {
            "now" => Val::Num(self.now_ms()),
            "timer_set" => {
                self.timers.insert(
                    wnum(a, 0) as u32,
                    (
                        self.now_ms() + wnum(a, 1).max(0.0),
                        a.get(2)
                            .is_some_and(|v| v.as_bool())
                            .then(|| wnum(a, 1).max(1.0)),
                    ),
                );
                Val::Undefined
            }
            "timer_clear" => {
                self.timers.remove(&(wnum(a, 0) as u32));
                Val::Undefined
            }
            "random" => Val::Num(self.rand()),
            "console" => {
                let (level, msg) = (ws_arg(a, 0), ws_arg(a, 1));
                self.console.push((level, msg));
                if self.console.len() > 2000 {
                    self.console.drain(..500);
                }
                Val::Undefined
            }
            "base64_encode_str" => Val::Str(crate::host::base64_encode(ws_arg(a, 0).as_bytes())),
            "fetch_start" => {
                let headers: Vec<(String, String)> = match a.get(3) {
                    Some(Val::List(l)) => l
                        .chunks(2)
                        .filter_map(|c| {
                            Some((
                                c.first()?.as_str()?.to_string(),
                                c.get(1)?.as_str()?.to_string(),
                            ))
                        })
                        .collect(),
                    _ => vec![],
                };
                let body = a
                    .get(4)
                    .and_then(|v| v.as_str())
                    .and_then(crate::host::base64_decode);
                self.start_fetch(
                    wnum(a, 0) as u32,
                    ws_arg(a, 1),
                    ws_arg(a, 2).to_uppercase(),
                    headers,
                    body,
                );
                Val::Undefined
            }
            "worker_post" => {
                let _ = self.event_tx.send(WorkerEvent::Message {
                    worker: self.worker_id,
                    json: ws_arg(a, 0),
                });
                Val::Undefined
            }
            "worker_error" => {
                let _ = self.event_tx.send(WorkerEvent::Error {
                    worker: self.worker_id,
                    message: ws_arg(a, 0),
                });
                Val::Undefined
            }
            "worker_close" => {
                self.closed = true;
                Val::Undefined
            }
            other => {
                return Err(format!(
                    "'{other}' is not available in dedicated workers (no DOM access)"
                ))
            }
        })
    }
}

/// Run one JS task under the worker's execution budget (mirrors the main runaway-script guard).
fn worker_task<T>(js: &mut QuickJsEngine, f: impl FnOnce(&mut QuickJsEngine) -> T) -> T {
    js.set_execution_deadline(Some(WORKER_TASK_BUDGET));
    let r = f(js);
    js.clear_execution_deadline();
    r
}

#[allow(clippy::too_many_arguments)]
fn worker_main(
    id: u32,
    key: String,
    source: String,
    is_module: bool,
    url: String,
    modules: HashMap<String, String>,
    network: bool,
    tx: Sender<WorkerEvent>,
    rx: Receiver<WorkerCommand>,
) {
    let done = |tx: &Sender<WorkerEvent>| {
        let _ = tx.send(WorkerEvent::Closed { worker: id });
    };
    let host = Rc::new(RefCell::new(WorkerHost::new(id, network, tx.clone())));
    let mut js = match QuickJsEngine::new(host.clone()) {
        Ok(js) => js,
        Err(e) => {
            let _ = tx.send(WorkerEvent::Error {
                worker: id,
                message: format!("worker runtime init failed: {e}"),
            });
            done(&tx);
            return;
        }
    };
    let prelude = format!("(function(global){{'use strict';\nconst N = global.__native;\ndelete global.__native;\n{EVENTS_JS}\n{WORKER_JS}\n}})(globalThis);");
    if let Err(e) = worker_task(&mut js, |js| {
        js.eval_script("rendrix:worker-prelude", &prelude)
    }) {
        let _ = tx.send(WorkerEvent::Error {
            worker: id,
            message: format!("worker prelude failed: {e}"),
        });
        done(&tx);
        return;
    }
    for (name, src) in &modules {
        js.add_module(name, src.clone());
    }
    // A stuck initial evaluation must not hang shutdown: it runs under the task budget.
    let started = if is_module {
        js.add_module(&key, source);
        worker_task(&mut js, |js| js.eval_module(&key))
    } else {
        worker_task(&mut js, |js| js.eval_script(&url, &source))
    };
    if let Err(e) = started {
        let _ = tx.send(WorkerEvent::Error {
            worker: id,
            message: e.to_string(),
        });
    }
    let _ = worker_task(&mut js, |js| js.run_jobs());
    let startup_logs = host.borrow_mut().take_console();
    for msg in startup_logs {
        let _ = tx.send(WorkerEvent::Console {
            worker: id,
            level: msg.0,
            text: msg.1,
        });
    }
    for r in worker_task(&mut js, |js| js.take_rejections()) {
        let _ = tx.send(WorkerEvent::Error {
            worker: id,
            message: format!("Unhandled promise rejection: {r}"),
        });
    }

    loop {
        // 1. Incoming posts (non-blocking drain; each dispatch is its own budgeted task).
        let mut exit = false;
        while let Ok(cmd) = rx.try_recv() {
            match cmd {
                WorkerCommand::Post(json) => {
                    let r = worker_task(&mut js, |js| {
                        js.call_global("__rendrix_workerDispatch", vec![Val::Str(json)])
                    });
                    if let Err(e) = r {
                        let _ = tx.send(WorkerEvent::Error {
                            worker: id,
                            message: e.to_string(),
                        });
                    }
                    let _ = worker_task(&mut js, |js| js.run_jobs());
                }
                WorkerCommand::Terminate => {
                    exit = true;
                    break;
                }
            }
        }
        if exit || host.borrow().closed {
            break;
        }
        // 2. Due timers.
        let now = host.borrow().now_ms();
        let due = host.borrow_mut().take_due_timers(now);
        for tid in due {
            let r = worker_task(&mut js, |js| {
                js.call_global("__rendrix_workerTimer", vec![Val::Num(tid as f64)])
            });
            if let Err(e) = r {
                let _ = tx.send(WorkerEvent::Error {
                    worker: id,
                    message: e.to_string(),
                });
            }
            let _ = worker_task(&mut js, |js| js.run_jobs());
        }
        // 3. Completed fetches.
        let fetches = host.borrow_mut().poll_fetches();
        for f in fetches {
            let mut flat = Vec::new();
            for (k, v) in &f.headers {
                flat.push(Val::Str(k.clone()));
                flat.push(Val::Str(v.clone()));
            }
            let r = worker_task(&mut js, |js| {
                js.call_global(
                    "__rendrix_workerFetchDone",
                    vec![
                        Val::Num(f.id as f64),
                        Val::Num(f.status as f64),
                        Val::Str(f.status_text.clone()),
                        Val::List(flat),
                        Val::Str(crate::host::base64_encode(&f.body)),
                        f.error.clone().map(Val::Str).unwrap_or(Val::Undefined),
                    ],
                )
            });
            if let Err(e) = r {
                let _ = tx.send(WorkerEvent::Error {
                    worker: id,
                    message: e.to_string(),
                });
            }
            let _ = worker_task(&mut js, |js| js.run_jobs());
        }
        // 4. Rejections + console, every turn.
        for r in worker_task(&mut js, |js| js.take_rejections()) {
            let _ = tx.send(WorkerEvent::Error {
                worker: id,
                message: format!("Unhandled promise rejection: {r}"),
            });
        }
        let logs = host.borrow_mut().take_console();
        for (level, text) in logs {
            let _ = tx.send(WorkerEvent::Console {
                worker: id,
                level,
                text,
            });
        }
        if host.borrow().closed {
            break;
        }
        // 5. Park briefly (bounded — `Terminate` and `close()` are noticed within ~8 ms), also
        //    picking up a command that arrived while pumping so the loop stays responsive.
        match rx.recv_timeout(WORKER_PARK) {
            Ok(WorkerCommand::Post(json)) => {
                let r = worker_task(&mut js, |js| {
                    js.call_global("__rendrix_workerDispatch", vec![Val::Str(json)])
                });
                if let Err(e) = r {
                    let _ = tx.send(WorkerEvent::Error {
                        worker: id,
                        message: e.to_string(),
                    });
                }
                let _ = worker_task(&mut js, |js| js.run_jobs());
            }
            Ok(WorkerCommand::Terminate) => break,
            Err(_) => {}
        }
    }
    done(&tx);
}
