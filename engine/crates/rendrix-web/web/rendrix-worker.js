// Rendrix web worker: hosts the wasm engine and runs the app's JavaScript.
//
// The app's prelude replaces `window`, `document`, `console`, `fetch`, timers, `postMessage`, ...
// on this worker's global. Everything this file needs from the real worker environment is captured
// first, before the engine starts, and used only through these bindings.

import init, { start } from './rendrix_web.js';

// Kick off the WASM download/compile at module evaluation, in parallel with the page fetching
// the app bundle. Previously the module only called `init()` after the bundle had arrived,
// serialising the two slowest parts of startup. The rejection is stored and re-thrown by the
// 'init' handler so it still reaches the page as an error event.
let wasmError = null;
const wasmReady = init().catch((e) => { wasmError = e; });

const post = self.postMessage.bind(self);
const addMessageListener = self.addEventListener.bind(self);
const realSetTimeout = self.setTimeout.bind(self);
const realClearTimeout = self.clearTimeout.bind(self);
const realRaf = typeof self.requestAnimationFrame === 'function' ? self.requestAnimationFrame.bind(self) : null;
const realLog = self.console.error.bind(self.console);

let app = null;
let booting = false;
const early = [];

// ---- frame loop ---------------------------------------------------------------------------

let framePending = false;
let wakeTimer = null;
let settle = 0; // frames to keep running after input: promise jobs only run once control returns

function requestFrame() {
  if (framePending || !app) return;
  framePending = true;
  if (realRaf) realRaf(tick); else realSetTimeout(() => tick(performance.now()), 16);
}

function wakeIn(ms) {
  if (wakeTimer !== null) realClearTimeout(wakeTimer);
  wakeTimer = realSetTimeout(() => { wakeTimer = null; requestFrame(); }, Math.max(0, ms));
}

const last = {};
// Scalars compare directly; only the two array fields need a structural compare. This used to
// JSON.stringify every field on every frame (the worker's hot path).
function changed(key, value) {
  if (last[key] === value) return false;
  last[key] = value;
  return true;
}
function samePair(a, b) {
  return !!a && !!b && a.length === b.length && a.every((v, i) => v === b[i]);
}
function sameRects(a, b) {
  if (!a || !b || a.length !== b.length) return false;
  for (let i = 0; i < a.length; i++) {
    const x = a[i], y = b[i];
    if (x.length !== y.length) return false;
    for (let j = 0; j < x.length; j++) if (x[j] !== y[j]) return false;
  }
  return true;
}

function tick() {
  framePending = false;
  if (!app) return;
  let report;
  try {
    report = JSON.parse(app.frame());
  } catch (e) {
    post({ t: 'error', message: String(e && e.message || e) });
    return;
  }
  const out = {};
  let dirty = false;
  if (report.cursor && changed('cursor', report.cursor)) { out.cursor = report.cursor; dirty = true; }
  if (changed('textInput', report.textInput)) { out.textInput = report.textInput; dirty = true; }
  if (!samePair(last.caret, report.caret)) { last.caret = report.caret; out.caret = report.caret; dirty = true; }
  if (report.title != null && changed('title', report.title)) { out.title = report.title; dirty = true; }
  if (report.textRects && !sameRects(last.textRects, report.textRects)) { last.textRects = report.textRects; out.textRects = report.textRects; dirty = true; }
  if (report.clipboard != null) { out.clipboard = report.clipboard; dirty = true; }
  if (report.external && report.external.length) { out.external = report.external; dirty = true; }
  if (report.quit) { out.quit = true; dirty = true; }
  if (report.repainted) { out.painted = true; dirty = true; }
  if (dirty) post({ t: 'frame', ...out });
  if (report.quit) { app = null; return; }
  if (settle > 0) { settle--; requestFrame(); }
  else if (report.continuous) requestFrame();
  else if (report.nextWakeMs != null) wakeIn(report.nextWakeMs);
}

// ---- messages from the page ---------------------------------------------------------------

function handle(m) {
  switch (m.t) {
    case 'ptr':
      if (m.k === 'move') app.pointer_move(m.x, m.y);
      else if (m.k === 'down') app.pointer_down(m.x, m.y, m.b);
      else app.pointer_up(m.x, m.y, m.b);
      break;
    case 'wheel': app.wheel(m.x, m.y, m.dx, m.dy); break;
    case 'touch':
      if (m.k === 'start') app.touch_start(m.id, m.x, m.y);
      else if (m.k === 'move') app.touch_move(m.id, m.x, m.y);
      else if (m.k === 'end') app.touch_end(m.id, m.x, m.y);
      else app.touch_cancel(m.id);
      break;
    case 'key':
      if (m.down) app.key_down(m.key, m.code, !!m.repeat, !!m.ctrl, !!m.shift, !!m.alt, !!m.meta);
      else app.key_up(m.key, m.code, !!m.ctrl, !!m.shift, !!m.alt, !!m.meta);
      break;
    case 'text': app.text_commit(m.text); break;
    case 'paste': app.paste(m.text); break;
    case 'resize': app.resize(m.w, m.h, m.dpr); break;
    case 'focus': app.set_window_focused(!!m.v); break;
    case 'env':
      if (m.safe) app.set_safe_area(m.safe[0], m.safe[1], m.safe[2], m.safe[3]);
      if (m.kb !== undefined) app.set_keyboard_height(m.kb);
      if (m.dark !== undefined) app.set_dark(!!m.dark);
      if (m.touchPoints !== undefined) app.set_max_touch_points(m.touchPoints);
      break;
    case 'pad': app.set_gamepad(m.i, m.id, Float32Array.from(m.buttons), Float32Array.from(m.axes)); break;
    case 'padOff': app.clear_gamepad(m.i); break;
    case 'eval': post({ t: 'evalResult', id: m.id, value: app.eval(m.expr) }); break;
    default: break;
  }
  settle = Math.max(settle, 3);
  requestFrame();
}

addMessageListener('message', async (e) => {
  const m = e.data;
  if (!m || typeof m !== 'object') return;
  if (m.t === 'init') {
    if (booting) return;
    booting = true;
    try {
      await wasmReady;
      if (wasmError) throw wasmError;
      app = await start(m.canvas, m.bundle, m.w, m.h, m.dpr);
      post({ t: 'ready', adapter: app.adapter() });
      for (const q of early.splice(0)) handle(q);
      settle = 5;
      requestFrame();
    } catch (err) {
      realLog('[rendrix] start failed', err);
      post({ t: 'error', message: String(err && err.message || err) });
    }
    return;
  }
  if (app) { try { handle(m); } catch (err) { post({ t: 'error', message: String(err && err.message || err) }); } }
  else early.push(m);
});
