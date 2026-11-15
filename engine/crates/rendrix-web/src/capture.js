// Browser primitives for the Rendrix wasm driver.
//
// The app's JavaScript runs in the *same* realm as this file and installs its own `window`,
// `document`, `console`, `fetch`, `setTimeout`, `crypto`, ... over the worker globals. Everything
// the driver itself needs from the browser is therefore captured here, at module-evaluation time
// (before the prelude runs), and used through these bindings only.

const g = globalThis;
const bind = (o, k) => (o && typeof o[k] === 'function' ? o[k].bind(o) : null);

const realFetch = bind(g, 'fetch');
const realCrypto = g.crypto;
// The app prelude replaces `URL` and `Blob` with its own implementations, so the browser
// originals (needed for module Blob URLs below) are captured before the prelude runs.
const realCreateObjectURL = g.URL ? bind(g.URL, 'createObjectURL') : null;
const RealBlob = g.Blob;
// `doFetch` builds its headers with the browser original: the prelude's `Headers` shim would be
// silently dropped as a fetch init (it exposes no own enumerable properties).
const RealHeaders = g.Headers;
const realPost = bind(g, 'postMessage');
const realAdd = bind(g, 'addEventListener');
const realConsole = {
  log: bind(g.console, 'log'), info: bind(g.console, 'info'), warn: bind(g.console, 'warn'),
  error: bind(g.console, 'error'), debug: bind(g.console, 'debug'),
};

export function consoleLog(level, msg) {
  (realConsole[level] || realConsole.log)(msg);
  // Worker console output is awkward to reach; mirror it to the page (`controller.logs`, `console` event).
  if (realPost) realPost({ t: 'console', level, msg: String(msg) });
}

/** Uniform f64 in [0, 1) from the browser's CSPRNG (53 random bits). */
export function randomF64() {
  const a = new Uint32Array(2);
  realCrypto.getRandomValues(a);
  return ((a[0] >>> 5) * 67108864 + (a[1] >>> 6)) / 9007199254740992;
}

/** Blob URL for a module source (see the captured `URL`/`Blob` above). */
export function blobUrl(source) {
  if (!realCreateObjectURL || !RealBlob) throw new Error('module loading needs URL.createObjectURL');
  return realCreateObjectURL(new RealBlob([source], { type: 'text/javascript' }));
}

/** `(...args) => call(args)`: wasm-bindgen closures are fixed-arity, `__native` is variadic. */
export function makeNative(call) {
  return (...a) => call(a);
}

/** Unhandled promise rejections (the page-level event; the app cannot swallow it). */
export function installRejectionListener(cb) {
  if (!realAdd) return;
  realAdd('unhandledrejection', (e) => {
    e.preventDefault();
    const r = e.reason;
    cb(r && r.stack ? String(r.message) + '\n' + String(r.stack) : String(r));
  });
}

/** Run source in the global scope (indirect eval). Returns undefined; throws on error. */
export function evalGlobal(source) {
  (0, eval)(source);
}

export function callGlobal(name, args) {
  const f = g[name];
  if (typeof f !== 'function') throw new Error('global function ' + name + ' is not defined');
  return f.apply(g, args);
}

export function errorInfo(e) {
  if (e && typeof e === 'object') return [String(e.message === undefined ? e : e.message), e.stack ? String(e.stack) : ''];
  return [String(e), ''];
}

/** HTTP via the browser. Resolves `{status, statusText, headers: [k, v, ...], body: Uint8Array}`. */
export async function doFetch(url, method, headersFlat, body) {
  if (!realFetch) throw new Error('fetch is not available');
  if (!RealHeaders) throw new Error('fetch is not available');
  const headers = new RealHeaders();
  for (let i = 0; i + 1 < headersFlat.length; i += 2) {
    try { headers.append(headersFlat[i], headersFlat[i + 1]); } catch (_) { /* forbidden header names are dropped */ }
  }
  const init = { method, headers };
  if (body && method !== 'GET' && method !== 'HEAD') init.body = body;
  const r = await realFetch(url, init);
  const buf = new Uint8Array(await r.arrayBuffer());
  const flat = [];
  r.headers.forEach((v, k) => flat.push(k, v));
  return { status: r.status, statusText: r.statusText, headers: flat, body: buf };
}

// ----- ES modules from the bundle's module map ------------------------------------------------

/** Same resolution as `rendrix_js::engine_trait::resolve_module`. */
function resolve(map, base, name) {
  let joined;
  if (name.startsWith('./') || name.startsWith('../')) {
    const i = base.lastIndexOf('/');
    const parts = i < 0 ? [] : base.slice(0, i).split('/');
    for (const seg of name.split('/')) {
      if (seg === '.' || seg === '') continue;
      if (seg === '..') parts.pop(); else parts.push(seg);
    }
    joined = parts.join('/');
  } else {
    joined = name.replace(/^\/+/, '');
  }
  for (const c of [joined, joined + '.js', joined + '/index.js', joined + '.mjs']) if (map.has(c)) return c;
  return null;
}

const DYNAMIC_IMPORT = /(^|[^.\w$])import\s*\(/g;
const SPEC_TAIL = /(?:\bfrom|\bimport)\s*$/;

/**
 * Rewrite import specifiers in module source. A small tokenizer (code / string / template /
 * comment) leaves comments and ordinary strings untouched: only a string literal directly after
 * `from` or `import` is a specifier, and `import(` in code becomes a dynamic-import call.
 * `onSpec(spec)` returns the replacement specifier, or null to keep it.
 */
function rewriteImports(src, onSpec, dynBase) {
  let out = '';
  let i = 0;
  let codeStart = 0;
  const n = src.length;
  const flush = (end) => {
    if (end > codeStart) {
      out += src.slice(codeStart, end).replace(DYNAMIC_IMPORT, (m, pre) => pre + 'globalThis.__rendrix_dynImport(' + JSON.stringify(dynBase) + ', ');
    }
  };
  while (i < n) {
    const c = src[i];
    if (c === '/' && src[i + 1] === '/') {
      flush(i);
      let j = src.indexOf('\n', i);
      if (j < 0) j = n;
      out += src.slice(i, j);
      i = codeStart = j;
    } else if (c === '/' && src[i + 1] === '*') {
      flush(i);
      let j = src.indexOf('*/', i + 2);
      j = j < 0 ? n : j + 2;
      out += src.slice(i, j);
      i = codeStart = j;
    } else if (c === '"' || c === "'" || c === '`') {
      flush(i);
      let j = i + 1;
      while (j < n && src[j] !== c) { if (src[j] === '\\') j++; j++; }
      j = Math.min(j + 1, n);
      const lit = src.slice(i, j);
      let repl = null;
      if (c !== '`' && SPEC_TAIL.test(out.slice(-24))) repl = onSpec(lit.slice(1, -1));
      out += repl === null ? lit : c + repl + c;
      i = codeStart = j;
    } else {
      i++;
    }
  }
  flush(n);
  return out;
}

/**
 * Runs the bundle's ES modules in the browser. Each module becomes a Blob URL with its static
 * import specifiers rewritten to its dependencies' Blob URLs (post-order), so the browser's own
 * module loader does linking, TLA and live bindings. Literal and computed `import()` go through
 * `__rendrix_dynImport`, which resolves against the same map at call time.
 *
 * Limitation: static import *cycles* cannot be expressed with Blob URLs and are reported as errors.
 */
export class ModuleLoader {
  constructor() {
    this.modules = new Map();
    this.urls = new Map();
    this.loaded = new Map();
    this.chain = Promise.resolve();
    this.inflight = 0;
    this.errors = [];
    g.__rendrix_dynImport = (base, spec) => {
      const r = resolve(this.modules, String(base), String(spec));
      if (r === null) return Promise.reject(new TypeError('Failed to resolve module specifier "' + spec + '"'));
      return this.load(r);
    };
  }

  add(name, source) {
    this.modules.set(name, source);
    this.urls.delete(name);
    this.loaded.delete(name);
  }

  url(name, stack = []) {
    const hit = this.urls.get(name);
    if (hit) return hit;
    if (stack.includes(name)) throw new Error('circular static imports are not supported on the web target: ' + [...stack, name].join(' -> '));
    const src = this.modules.get(name);
    if (src === undefined) throw new Error("module '" + name + "' not found");
    const st = [...stack, name];
    let out = rewriteImports(src, (spec) => {
      const r = resolve(this.modules, name, spec);
      return r === null ? null : this.url(r, st);
    }, name);
    out += '\n//# sourceURL=rendrix:///' + name;
    const u = blobUrl(out);
    this.urls.set(name, u);
    return u;
  }

  load(name) {
    let p = this.loaded.get(name);
    if (!p) {
      p = (async () => import(this.url(name)))();
      this.loaded.set(name, p);
    }
    return p;
  }

  /** Run `fn` after everything queued so far; failures are collected, never thrown. */
  enqueue(fn) {
    this.inflight++;
    this.chain = this.chain.then(fn).catch((e) => {
      this.errors.push(e && e.stack ? String(e.message) + '\n' + String(e.stack) : String(e));
    }).then(() => { this.inflight--; });
  }

  enqueueModule(name) {
    this.enqueue(() => this.load(name));
  }

  /** Call a global after queued modules finished, then flush DOM mutation delivery. */
  enqueueCall(name) {
    this.enqueue(() => {
      callGlobal(name, []);
      if (typeof g.__rendrix_afterTask === 'function') g.__rendrix_afterTask();
    });
  }

  get pending() { return this.inflight; }

  takeErrors() {
    const e = this.errors;
    this.errors = [];
    return e;
  }
}
