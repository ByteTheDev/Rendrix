// Rendrix on the web: mount an app into an element.
//
//   import { mount } from './rendrix-web.js';
//   const app = await mount(document.getElementById('root'), './app.json');
//
// This file runs on the page. It owns the real <canvas>, forwards pointer / touch / wheel /
// keyboard / IME / clipboard / gamepad input and environment (size, pixel ratio, safe areas,
// on-screen keyboard, colour scheme) to the engine worker, and applies what the engine asks for
// back (cursor, soft keyboard, title, clipboard writes, external links).

const COARSE = () => matchMedia('(pointer: coarse)').matches;

// Render-budget defaults. A 3x-DPI phone paints 9x the pixels of a 1x screen, and a large
// high-DPI window can exceed 20M pixels/frame. Both are wasted work for most UIs; games get
// the biggest win. Override with `mount(el, bundle, { maxDpr, maxPixels })` and adjust at
// runtime with `controller.setQuality(...)`.
const DEFAULT_MAX_DPR = 2;
const DEFAULT_MAX_PIXELS = 8_000_000;

export function budgetDpr(w, h, deviceDpr, maxDpr, maxPixels) {
  let d = Math.min(deviceDpr || 1, Math.max(1, maxDpr));
  const pixels = w * h * d * d;
  // Never drop below 1x: downscaling below device pixels makes text soft even on 1x displays.
  if (pixels > maxPixels) d = Math.max(1, d * Math.sqrt(maxPixels / pixels));
  return d;
}

export async function mount(target, bundle, options = {}) {
  const json = typeof bundle === 'string' && !bundle.trim().startsWith('{') ? await (await fetch(bundle)).text() : (typeof bundle === 'string' ? bundle : JSON.stringify(bundle));
  const workerUrl = options.workerUrl || new URL('./rendrix-worker.js', import.meta.url);
  const events = new EventTarget();
  const quality = {
    maxDpr: Math.max(1, options.maxDpr ?? DEFAULT_MAX_DPR),
    maxPixels: Math.max(1, options.maxPixels ?? DEFAULT_MAX_PIXELS),
  };

  const host = document.createElement('div');
  host.style.cssText = 'position:relative;width:100%;height:100%;overflow:hidden;';
  const canvas = document.createElement('canvas');
  // `touch-action:none`: the engine implements panning/fling itself (and honours CSS `touch-action`).
  canvas.style.cssText = 'display:block;width:100%;height:100%;touch-action:none;outline:none;-webkit-tap-highlight-color:transparent;user-select:none;-webkit-user-select:none;';
  canvas.tabIndex = 0;
  host.appendChild(canvas);

  // Hidden text input: owns IME composition, the soft keyboard and paste.
  const input = document.createElement('textarea');
  input.setAttribute('autocapitalize', 'off');
  input.setAttribute('autocomplete', 'off');
  input.setAttribute('autocorrect', 'off');
  input.setAttribute('spellcheck', 'false');
  input.setAttribute('aria-hidden', 'true');
  input.tabIndex = -1;
  // 16px keeps iOS from zooming into the field.
  input.style.cssText = 'position:absolute;left:0;top:0;width:1px;height:1px;padding:0;border:0;opacity:0;font-size:16px;resize:none;overflow:hidden;caret-color:transparent;background:transparent;color:transparent;';
  host.appendChild(input);

  // Safe-area probe: reading `env()` needs a real element.
  const probe = document.createElement('div');
  probe.style.cssText = 'position:absolute;visibility:hidden;pointer-events:none;left:0;top:0;width:0;height:0;padding:env(safe-area-inset-top,0px) env(safe-area-inset-right,0px) env(safe-area-inset-bottom,0px) env(safe-area-inset-left,0px);';
  host.appendChild(probe);

  target.appendChild(host);

  const worker = new Worker(workerUrl, { type: 'module', name: 'rendrix' });
  const send = (m, transfer) => worker.postMessage(m, transfer || []);
  const coarse = COARSE();
  let ready = false;
  let wantsText = false;
  let textRects = [];
  let destroyed = false;
  const cleanup = [];
  const on = (el, ev, fn, opts) => { el.addEventListener(ev, fn, opts); cleanup.push(() => el.removeEventListener(ev, fn, opts)); };

  // getBoundingClientRect() forces layout. Cache it and refresh only when something can move
  // the canvas, instead of on every pointermove (mice can fire 1000 events/second).
  let rect = null;
  const refreshRect = () => { rect = canvas.getBoundingClientRect(); };
  const sizeNow = () => {
    if (!rect) refreshRect();
    const w = Math.max(1, Math.round(rect.width));
    const h = Math.max(1, Math.round(rect.height));
    return { w, h, dpr: budgetDpr(w, h, window.devicePixelRatio || 1, quality.maxDpr, quality.maxPixels) };
  };
  const pos = (e) => { if (!rect) refreshRect(); return [e.clientX - rect.left, e.clientY - rect.top]; };

  // ---- environment -------------------------------------------------------------------------
  const safeArea = () => {
    const c = getComputedStyle(probe);
    return [parseFloat(c.paddingTop) || 0, parseFloat(c.paddingRight) || 0, parseFloat(c.paddingBottom) || 0, parseFloat(c.paddingLeft) || 0];
  };
  const keyboardHeight = () => {
    const vv = window.visualViewport;
    if (!vv || document.activeElement !== input) return 0;
    const h = window.innerHeight - vv.height - vv.offsetTop;
    return h > 80 ? Math.round(h) : 0; // small deltas are browser chrome, not a keyboard
  };
  const pushEnv = (extra = {}) => send({ t: 'env', safe: safeArea(), kb: keyboardHeight(), dark: matchMedia('(prefers-color-scheme: dark)').matches, touchPoints: navigator.maxTouchPoints || 0, ...extra });
  const pushSize = () => { const s = sizeNow(); send({ t: 'resize', ...s }); pushEnv(); };

  const refresh = () => { refreshRect(); if (ready) pushSize(); };
  const ro = new ResizeObserver(refresh);
  ro.observe(canvas);
  cleanup.push(() => ro.disconnect());
  on(window, 'resize', refresh);
  on(window, 'orientationchange', refresh);
  on(window, 'scroll', refreshRect, true); // capture: any ancestor scroll moves the canvas
  if (window.visualViewport) { on(window.visualViewport, 'resize', () => ready && pushEnv()); on(window.visualViewport, 'scroll', () => ready && pushEnv()); }
  const mq = matchMedia('(prefers-color-scheme: dark)');
  on(mq, 'change', () => ready && pushEnv());
  on(window, 'focus', () => send({ t: 'focus', v: true }));
  on(window, 'blur', () => send({ t: 'focus', v: false }));

  // ---- pointer, touch -----------------------------------------------------------------------
  const touchIds = new Map();
  const touchId = (pid) => { if (!touchIds.has(pid)) { let n = 0; const used = new Set(touchIds.values()); while (used.has(n)) n++; touchIds.set(pid, n); } return touchIds.get(pid); };
  const inTextRect = (x, y) => textRects.some((r) => x >= r[0] && y >= r[1] && x < r[0] + r[2] && y < r[1] + r[3]);

  const focusInput = () => { if (document.activeElement !== input) input.focus({ preventScroll: true }); };

  on(canvas, 'pointerdown', (e) => {
    canvas.setPointerCapture(e.pointerId);
    refreshRect();
    const [x, y] = pos(e);
    if (e.pointerType === 'touch') {
      send({ t: 'touch', k: 'start', id: touchId(e.pointerId), x, y });
      // iOS only opens the keyboard from inside a user gesture: decide here, from the last known text-field rects.
      if (coarse && inTextRect(x, y)) focusInput();
    } else {
      if (!coarse) focusInput();
      send({ t: 'ptr', k: 'down', x, y, b: e.button });
    }
    e.preventDefault();
  });
  on(canvas, 'pointermove', (e) => {
    const [x, y] = pos(e);
    if (e.pointerType === 'touch') { if (touchIds.has(e.pointerId)) send({ t: 'touch', k: 'move', id: touchId(e.pointerId), x, y }); }
    else send({ t: 'ptr', k: 'move', x, y });
  });
  const touchUp = (e, cancel) => {
    const [x, y] = pos(e);
    if (e.pointerType === 'touch' && touchIds.has(e.pointerId)) {
      send({ t: 'touch', k: cancel ? 'cancel' : 'end', id: touchId(e.pointerId), x, y });
      touchIds.delete(e.pointerId);
    } else if (e.pointerType !== 'touch') {
      send({ t: 'ptr', k: 'up', x, y, b: e.button });
    }
  };
  on(canvas, 'pointerup', (e) => touchUp(e, false));
  on(canvas, 'pointercancel', (e) => touchUp(e, true));
  on(canvas, 'pointerleave', (e) => { if (e.pointerType === 'mouse' && e.buttons === 0) send({ t: 'ptr', k: 'move', x: -1, y: -1 }); });
  on(canvas, 'contextmenu', (e) => e.preventDefault());
  on(canvas, 'wheel', (e) => {
    const [x, y] = pos(e);
    const unit = e.deltaMode === 1 ? 48 : e.deltaMode === 2 ? canvas.clientHeight : 1;
    send({ t: 'wheel', x, y, dx: e.deltaX * unit, dy: e.deltaY * unit });
    if (!e.ctrlKey) e.preventDefault();
  }, { passive: false });

  // ---- keyboard, text, IME, clipboard ----------------------------------------------------------
  // Keys the engine handles itself; letting the browser act on them would move page focus or scroll.
  const OWNED = new Set(['Tab', 'ArrowLeft', 'ArrowRight', 'ArrowUp', 'ArrowDown', 'Home', 'End', 'PageUp', 'PageDown', 'Enter', 'Backspace', 'Delete', ' ', 'Escape']);
  const keyMsg = (e, down) => ({ t: 'key', down, key: e.key, code: e.code, repeat: e.repeat, ctrl: e.ctrlKey, shift: e.shiftKey, alt: e.altKey, meta: e.metaKey });
  let composing = false;
  const forwardKey = (e, down) => {
    if (composing || e.isComposing || e.key === 'Process') return;
    const clip = (e.ctrlKey || e.metaKey) && /^(c|x|v)$/i.test(e.key);
    if (clip && e.key.toLowerCase() === 'v') return; // `paste` event carries the text
    send(keyMsg(e, down));
    if (OWNED.has(e.key) && !(e.key === ' ' && !wantsText && false)) e.preventDefault();
  };
  on(input, 'keydown', (e) => forwardKey(e, true));
  on(input, 'keyup', (e) => forwardKey(e, false));
  on(canvas, 'keydown', (e) => forwardKey(e, true));
  on(canvas, 'keyup', (e) => forwardKey(e, false));
  on(input, 'compositionstart', () => { composing = true; });
  on(input, 'compositionend', (e) => { composing = false; if (e.data) send({ t: 'text', text: e.data }); input.value = ''; });
  on(input, 'beforeinput', (e) => {
    // Committed text only: composition results arrive with `compositionend`.
    if (e.isComposing || composing) return;
    if (e.inputType === 'insertText' && e.data) { send({ t: 'text', text: e.data }); e.preventDefault(); }
    else if (e.inputType === 'insertFromPaste' || e.inputType === 'insertLineBreak' || e.inputType === 'insertParagraph') e.preventDefault();
  });
  on(input, 'paste', (e) => { const text = e.clipboardData ? e.clipboardData.getData('text/plain') : ''; send({ t: 'paste', text }); e.preventDefault(); });
  on(input, 'blur', () => { if (ready) pushEnv(); });
  on(input, 'focus', () => { if (ready) pushEnv(); });
  on(canvas, 'focus', () => { if (!coarse) focusInput(); });

  const applyTextInput = () => {
    if (coarse) { if (wantsText) focusInput(); else if (document.activeElement === input) input.blur(); }
  };

  // ---- gamepads (the Gamepad API is main-thread only) -------------------------------------------
  const padState = new Map();
  let padLoop = 0;
  const pollPads = () => {
    padLoop = 0;
    if (destroyed) return;
    const pads = navigator.getGamepads ? navigator.getGamepads() : [];
    let any = false;
    for (let i = 0; i < pads.length; i++) {
      const p = pads[i];
      if (p && p.connected) {
        any = true;
        const buttons = p.buttons.map((b) => b.value);
        const axes = [...p.axes];
        const key = JSON.stringify([p.id, buttons, axes]);
        if (padState.get(i) !== key) { padState.set(i, key); send({ t: 'pad', i, id: p.id, buttons, axes }); }
      } else if (padState.has(i)) { padState.delete(i); send({ t: 'padOff', i }); }
    }
    if (any || padState.size) padLoop = requestAnimationFrame(pollPads);
  };
  on(window, 'gamepadconnected', () => { if (!padLoop) padLoop = requestAnimationFrame(pollPads); });
  if (navigator.getGamepads && [...navigator.getGamepads()].some(Boolean)) padLoop = requestAnimationFrame(pollPads);

  // ---- messages from the engine ----------------------------------------------------------------
  const evalWaiters = new Map();
  let evalSeq = 0;
  // Lightweight, always-on frame telemetry so performance changes can be measured in a real
  // browser rather than guessed. `frameMs` is an EMA of the interval between painted frames.
  const stats = { frames: 0, fps: 0, frameMs: 0 };
  let lastPaint = 0;
  const result = new Promise((resolve, reject) => {
    worker.addEventListener('message', (e) => {
      const m = e.data;
      if (m.t === 'ready') {
        ready = true;
        controller.adapter = m.adapter;
        pushEnv();
        resolve(controller);
        events.dispatchEvent(Object.assign(new Event('ready'), { adapter: m.adapter }));
      } else if (m.t === 'frame') {
        if (m.cursor) canvas.style.cursor = m.cursor;
        if (m.title !== undefined && options.syncTitle !== false) document.title = m.title;
        if (m.textRects) textRects = m.textRects;
        if (m.caret) { input.style.left = Math.max(0, m.caret[0]) + 'px'; input.style.top = Math.max(0, m.caret[1]) + 'px'; }
        if (m.textInput !== undefined) { wantsText = m.textInput; applyTextInput(); }
        if (m.clipboard != null && navigator.clipboard) navigator.clipboard.writeText(m.clipboard).catch(() => {});
        if (m.external) for (const url of m.external) window.open(url, '_blank', 'noopener,noreferrer');
        if (m.quit) { events.dispatchEvent(new Event('quit')); }
        if (m.painted) {
          const now = performance.now();
          if (lastPaint) {
            const dt = now - lastPaint;
            stats.frameMs = stats.frameMs ? stats.frameMs * 0.9 + dt * 0.1 : dt;
            stats.fps = stats.frameMs > 0 ? 1000 / stats.frameMs : 0;
          }
          lastPaint = now;
          stats.frames++;
          events.dispatchEvent(new Event('frame'));
        }
      } else if (m.t === 'console') {
        controller.logs.push([m.level, m.msg]);
        if (controller.logs.length > 500) controller.logs.shift();
        events.dispatchEvent(Object.assign(new Event('console'), { level: m.level, message: m.msg }));
        if (options.echoConsole !== false) (console[m.level] || console.log)('[app]', m.msg);
      } else if (m.t === 'evalResult') {
        const w = evalWaiters.get(m.id); if (w) { evalWaiters.delete(m.id); w(m.value); }
      } else if (m.t === 'error') {
        events.dispatchEvent(Object.assign(new Event('error'), { message: m.message }));
        if (!ready) reject(new Error(m.message)); else console.error('[rendrix]', m.message);
      }
    });
    worker.addEventListener('error', (e) => { if (!ready) reject(new Error(e.message || 'worker failed to start')); });
  });

  const controller = {
    canvas,
    events,
    /** Adapter description from the worker (`WebApp::adapter`), set on `ready`. */
    adapter: '',
    /** Recent app console output as [level, message] pairs. */
    logs: [],
    /** Live render telemetry: { frames, fps, frameMs } for painted frames. */
    stats,
    /** Evaluate an expression inside the app; resolves to a string (objects are JSON). */
    eval(expr) { const id = ++evalSeq; return new Promise((res) => { evalWaiters.set(id, res); send({ t: 'eval', id, expr }); }); },
    /** Change the render budget at runtime (useful for games trading sharpness for frame rate). */
    setQuality(next = {}) {
      if (next.maxDpr != null) quality.maxDpr = Math.max(1, next.maxDpr);
      if (next.maxPixels != null) quality.maxPixels = Math.max(1, next.maxPixels);
      if (ready) pushSize();
      return { ...quality };
    },
    destroy() {
      destroyed = true;
      for (const c of cleanup.splice(0)) c();
      worker.terminate();
      host.remove();
    },
  };

  const s = sizeNow();
  const offscreen = canvas.transferControlToOffscreen();
  send({ t: 'init', canvas: offscreen, bundle: json, w: s.w, h: s.h, dpr: s.dpr }, [offscreen]);
  return result;
}
