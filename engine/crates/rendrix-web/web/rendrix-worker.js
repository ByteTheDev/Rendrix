// Rendrix web worker: hosts the wasm engine and runs the app's JavaScript.
//
// The app's prelude replaces `window`, `document`, `console`, `fetch`, timers, `postMessage`, ...
// on this worker's global. Everything this file needs from the real worker environment is captured
// first, before the engine starts, and used only through these bindings.

import init, { start } from './rendrix_web.js';

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
function changed(key, value) {
  const s = JSON.stringify(value);
  if (last[key] === s) return false;
  last[key] = s;
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
  if (report.cursor && changed('cursor', report.cursor)) out.cursor = report.cursor;
  if (changed('textInput', report.textInput)) out.textInput = report.textInput;
  if (changed('caret', report.caret)) out.caret = report.caret;
  if (report.title != null && changed('title', report.title)) out.title = report.title;
  if (report.textRects && changed('textRects', report.textRects)) out.textRects = report.textRects;
  if (report.clipboard != null) out.clipboard = report.clipboard;
  if (report.external && report.external.length) out.external = report.external;
  if (report.quit) out.quit = true;
  if (report.repainted) out.painted = true;
  if (Object.keys(out).length) post({ t: 'frame', ...out });
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
      await init();
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
