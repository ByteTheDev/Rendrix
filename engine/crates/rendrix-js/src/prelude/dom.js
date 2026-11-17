// ---------------------------------------------------------------------------------------------
// DOM: wrappers over the native arena DOM. A wrapper is just a handle (node id); identity is
// preserved through a weak cache so `el === el` and expando properties/listeners survive.
// ---------------------------------------------------------------------------------------------

const ID = ID_SYMBOL;
const wrapperCache = new Map();
const registry = typeof FinalizationRegistry === 'function'
  ? new FinalizationRegistry(id => {
      const r = wrapperCache.get(id);
      if (r && !r.deref()) wrapperCache.delete(id);
      N('release', id);
    })
  : null;

// Wrappers of nodes in the document are held strongly so JS-side identity and expando properties
// survive GC; wrappers of detached nodes are weak and let the native node be freed.
const strongWrappers = new Map();
let pendingUpgrade = [];
let lastEpoch = -1, lastReconcile = 0;
function adopt(obj, id) {
  Object.defineProperty(obj, ID, { value: id });
  N('retain', id);
  wrapperCache.set(id, new WeakRef(obj));
  if (registry) registry.register(obj, id);
  if (N('is_connected', id)) strongWrappers.set(id, obj); else pendingUpgrade.push(new WeakRef(obj));
  return obj;
}
function upgradePending() {
  if (!pendingUpgrade.length) return;
  const keep = [];
  for (const ref of pendingUpgrade) {
    const w = ref.deref(); if (!w) continue;
    if (strongWrappers.has(w[ID])) continue;
    if (N('is_connected', w[ID])) strongWrappers.set(w[ID], w); else keep.push(ref);
  }
  pendingUpgrade = keep;
}
function reconcileStrong() {
  const epoch = N('detach_epoch'); const now = N('now');
  if (epoch === lastEpoch || now - lastReconcile < 500) return;
  lastEpoch = epoch; lastReconcile = now;
  for (const [id, w] of strongWrappers) if (!N('is_connected', id)) { strongWrappers.delete(id); pendingUpgrade.push(new WeakRef(w)); }
}
function nodesFreed() { for (const id of N('take_freed')) { forgetListeners(id); strongWrappers.delete(id); wrapperCache.delete(id); } }

// Run a mutating native op and surface native errors as DOMException.
function mut(op, ...args) {
  try { return N(op, ...args); }
  catch (e) {
    const m = /^(\w+Error): ?([\s\S]*)$/.exec(e && e.message || '');
    if (m) throw new DOMException(m[2], m[1]);
    throw e;
  }
}

const kebab = s => s.replace(/[A-Z]/g, m => '-' + m.toLowerCase());
const camel = s => s.replace(/-([a-z])/g, (_, c) => c.toUpperCase());

class NodeList extends Array {
  static get [Symbol.species]() { return Array; }
  item(i) { return this[i] || null; }
}
class HTMLCollection extends NodeList {
  namedItem(name) { return this.find(e => e.id === name || e.getAttribute('name') === name) || null; }
}

function wrap(id) {
  if (id === null || id === undefined) return null;
  const r = wrapperCache.get(id);
  const hit = r && r.deref();
  if (hit) return hit;
  const type = N('node_type', id);
  let proto;
  if (type === 1) proto = elementClass(N('tag', id)).prototype;
  else if (type === 3) proto = Text.prototype;
  else if (type === 8) proto = Comment.prototype;
  else if (type === 9) proto = Document.prototype;
  else proto = DocumentFragment.prototype;
  return adopt(Object.create(proto), id);
}
const wrapAll = ids => ids.map(wrap);
const idOf = n => { if (!n || n[ID] === undefined) throw new TypeError('argument is not a Node'); return n[ID]; };

function toNode(x) { return x instanceof Node ? x : document.createTextNode(String(x)); }

// ----- Node -----------------------------------------------------------------------------------

class Node extends EventTarget {
  static ELEMENT_NODE = 1; static TEXT_NODE = 3; static COMMENT_NODE = 8; static DOCUMENT_NODE = 9; static DOCUMENT_FRAGMENT_NODE = 11;
  static DOCUMENT_POSITION_DISCONNECTED = 1; static DOCUMENT_POSITION_PRECEDING = 2; static DOCUMENT_POSITION_FOLLOWING = 4;
  static DOCUMENT_POSITION_CONTAINS = 8; static DOCUMENT_POSITION_CONTAINED_BY = 16;
  get nodeType() { return N('node_type', this[ID]); }
  get nodeName() {
    switch (this.nodeType) { case 1: return this.tagName; case 3: return '#text'; case 8: return '#comment'; case 9: return '#document'; default: return '#document-fragment'; }
  }
  get nodeValue() { return N('text_data', this[ID]); }
  set nodeValue(v) { if (this.nodeType === 3 || this.nodeType === 8) N('set_text_data', this[ID], String(v)); }
  get parentNode() { return wrap(N('parent', this[ID])); }
  get parentElement() { const p = this.parentNode; return p && p.nodeType === 1 ? p : null; }
  get firstChild() { return wrap(N('first_child', this[ID])); }
  get lastChild() { return wrap(N('last_child', this[ID])); }
  get nextSibling() { return wrap(N('next_sibling', this[ID])); }
  get previousSibling() { return wrap(N('prev_sibling', this[ID])); }
  get childNodes() { return NodeList.from(wrapAll(N('children', this[ID]))); }
  get isConnected() { return N('is_connected', this[ID]); }
  get ownerDocument() { return this.nodeType === 9 ? null : document; }
  get baseURI() { return location.href; }
  hasChildNodes() { return N('first_child', this[ID]) !== null; }
  appendChild(c) { mut('append_child', this[ID], idOf(c)); return c; }
  insertBefore(c, ref) { mut('insert_before', this[ID], idOf(c), ref ? idOf(ref) : null); return c; }
  removeChild(c) { mut('remove_child', this[ID], idOf(c)); return c; }
  replaceChild(n, o) { mut('replace_child', this[ID], idOf(n), idOf(o)); return o; }
  cloneNode(deep = false) { return wrap(N('clone_node', this[ID], !!deep)); }
  contains(o) { return !!o && N('contains', this[ID], idOf(o)); }
  getRootNode() { let n = this; while (n.parentNode) n = n.parentNode; return n; }
  isSameNode(o) { return this === o; }
  isEqualNode(o) { return !!o && this.nodeType === o.nodeType && (this.nodeType === 1 ? this.outerHTML === o.outerHTML : this.textContent === o.textContent); }
  normalize() {}
  compareDocumentPosition(o) {
    if (this === o) return 0;
    if (this.contains(o)) return 20;
    if (o.contains(this)) return 10;
    const all = N('query_all', document[ID], '*').map(wrap);
    const a = all.indexOf(this), b = all.indexOf(o);
    if (a < 0 || b < 0) return 1;
    return a < b ? 4 : 2;
  }
  get textContent() { return this.nodeType === 9 ? null : N('text_content', this[ID]); }
  set textContent(v) { N('set_text_content', this[ID], v == null ? '' : String(v)); }
  _eventParent() { const p = N('parent', this[ID]); return p === null ? null : wrap(p); }
}

class CharacterData extends Node {
  get data() { return N('text_data', this[ID]); }
  set data(v) { N('set_text_data', this[ID], String(v)); }
  get length() { return this.data.length; }
  get nextElementSibling() { let n = this.nextSibling; while (n && n.nodeType !== 1) n = n.nextSibling; return n; }
  get previousElementSibling() { let n = this.previousSibling; while (n && n.nodeType !== 1) n = n.previousSibling; return n; }
  appendData(s) { this.data += s; }
  substringData(o, c) { return this.data.substr(o, c); }
  remove() { const p = this.parentNode; if (p) p.removeChild(this); }
}
class Text extends CharacterData {
  constructor(data = '') { super(); adopt(this, N('create_text', String(data))); }
  get wholeText() { return this.data; }
}
class Comment extends CharacterData {
  constructor(data = '') { super(); adopt(this, N('create_comment', String(data))); }
}

// ParentNode / ChildNode mixins
function parentNodeMixin(P) {
  Object.defineProperties(P, {
    children: { get() { return HTMLCollection.from(wrapAll(N('children', this[ID])).filter(n => n.nodeType === 1)); }, configurable: true },
    childElementCount: { get() { return this.children.length; }, configurable: true },
    firstElementChild: { get() { return this.children[0] || null; }, configurable: true },
    lastElementChild: { get() { const c = this.children; return c[c.length - 1] || null; }, configurable: true },
  });
  P.querySelector = function (sel) { return wrap(mut('query', this[ID], String(sel))); };
  P.querySelectorAll = function (sel) { return NodeList.from(wrapAll(mut('query_all', this[ID], String(sel)))); };
  P.append = function (...nodes) { for (const n of nodes) this.appendChild(toNode(n)); };
  P.prepend = function (...nodes) { const first = this.firstChild; for (const n of nodes) this.insertBefore(toNode(n), first); };
  P.replaceChildren = function (...nodes) { N('set_text_content', this[ID], ''); this.append(...nodes); };
  P.getElementById = function (id) { return wrap(N('by_id', String(id))); };
}
function childNodeMixin(P) {
  P.remove = function () { const p = this.parentNode; if (p) p.removeChild(this); };
  P.before = function (...nodes) { const p = this.parentNode; if (p) for (const n of nodes) p.insertBefore(toNode(n), this); };
  P.after = function (...nodes) { const p = this.parentNode; if (!p) return; const next = this.nextSibling; for (const n of nodes) p.insertBefore(toNode(n), next); };
  P.replaceWith = function (...nodes) { const p = this.parentNode; if (!p) return; const next = this.nextSibling; p.removeChild(this); for (const n of nodes) p.insertBefore(toNode(n), next); };
}
childNodeMixin(CharacterData.prototype);

class DocumentFragment extends Node {
  constructor() { super(); adopt(this, N('create_fragment')); }
}
parentNodeMixin(DocumentFragment.prototype);

// ----- Range / Selection (minimal but functional) --------------------------------------
let designModeValue = 'off';

function nodeLength(n) {
  if (!n) return 0;
  if (n.nodeType === 3 || n.nodeType === 8) return (n.data || '').length;
  return n.childNodes ? n.childNodes.length : 0;
}
function textIn(node) {
  if (!node) return '';
  if (node.nodeType === 3) return node.data || '';
  if (node.nodeType === 1 || node.nodeType === 11 || node.nodeType === 9) {
    let s = '';
    for (const c of node.childNodes || []) s += textIn(c);
    return s;
  }
  return '';
}

class Range {
  constructor() { this.startContainer = null; this.startOffset = 0; this.endContainer = null; this.endOffset = 0; }
  get collapsed() { return this.startContainer === this.endContainer && this.startOffset === this.endOffset; }
  get commonAncestorContainer() {
    if (!this.startContainer || !this.endContainer) return null;
    if (this.startContainer === this.endContainer) return this.startContainer;
    // Walk up from start until it contains end.
    let n = this.startContainer;
    while (n) { if (n.contains && n.contains(this.endContainer)) return n; n = n.parentNode; }
    return document;
  }
  setStart(n, o) { this.startContainer = n; this.startOffset = Math.max(0, Math.min(o | 0, nodeLength(n))); if (!this.endContainer) { this.endContainer = n; this.endOffset = this.startOffset; } }
  setEnd(n, o) { this.endContainer = n; this.endOffset = Math.max(0, Math.min(o | 0, nodeLength(n))); if (!this.startContainer) { this.startContainer = n; this.startOffset = this.endOffset; } }
  setStartBefore(n) { const p = n.parentNode; this.setStart(p, [...(p.childNodes || [])].indexOf(n)); }
  setStartAfter(n) { const p = n.parentNode; this.setStart(p, [...(p.childNodes || [])].indexOf(n) + 1); }
  setEndBefore(n) { const p = n.parentNode; this.setEnd(p, [...(p.childNodes || [])].indexOf(n)); }
  setEndAfter(n) { const p = n.parentNode; this.setEnd(p, [...(p.childNodes || [])].indexOf(n) + 1); }
  selectNode(n) { this.setStartBefore(n); this.setEndAfter(n); }
  selectNodeContents(n) {
    this.startContainer = n; this.startOffset = 0;
    this.endContainer = n; this.endOffset = nodeLength(n);
  }
  collapse(toStart = false) {
    if (toStart) { this.endContainer = this.startContainer; this.endOffset = this.startOffset; }
    else { this.startContainer = this.endContainer; this.startOffset = this.endOffset; }
  }
  cloneRange() { const r = new Range(); r.startContainer = this.startContainer; r.startOffset = this.startOffset; r.endContainer = this.endContainer; r.endOffset = this.endOffset; return r; }
  toString() {
    if (!this.startContainer) return '';
    if (this.startContainer === this.endContainer && this.startContainer.nodeType === 3) {
      return (this.startContainer.data || '').slice(this.startOffset, this.endOffset);
    }
    // Best-effort: text of the common ancestor (covers selectAllChildren + simple ranges).
    const root = this.commonAncestorContainer;
    return textIn(root);
  }
}

const docSelectionRanges = [];
class Selection {
  get rangeCount() { return docSelectionRanges.length; }
  get anchorNode() { return docSelectionRanges.length ? docSelectionRanges[0].startContainer : null; }
  get focusNode() { return docSelectionRanges.length ? docSelectionRanges[0].endContainer : null; }
  get anchorOffset() { return docSelectionRanges.length ? docSelectionRanges[0].startOffset : 0; }
  get focusOffset() { return docSelectionRanges.length ? docSelectionRanges[0].endOffset : 0; }
  get isCollapsed() { return !docSelectionRanges.length || docSelectionRanges[0].collapsed; }
  getRangeAt(i) {
    if (i < 0 || i >= docSelectionRanges.length) throw new DOMException('Invalid range index', 'IndexSizeError');
    return docSelectionRanges[i];
  }
  addRange(r) { if (r instanceof Range) { docSelectionRanges.length = 0; docSelectionRanges.push(r.cloneRange()); } }
  removeAllRanges() { docSelectionRanges.length = 0; }
  removeRange(r) { const i = docSelectionRanges.indexOf(r); if (i >= 0) docSelectionRanges.splice(i, 1); }
  collapse(n, o = 0) { docSelectionRanges.length = 0; if (n) { const r = new Range(); r.selectNodeContents(n); r.collapse(true); try { r.setStart(n, o); r.collapse(true); } catch (_) {} docSelectionRanges.push(r); } }
  selectAllChildren(n) { const r = new Range(); r.selectNodeContents(n); docSelectionRanges.length = 0; docSelectionRanges.push(r); }
  deleteFromDocument() { this.toString(); this.removeAllRanges(); }
  toString() { return docSelectionRanges.map(r => { try { return r.toString(); } catch (_) { return ''; } }).join(''); }
}
const docSelection = new Selection();

// ----- Shadow DOM (lightweight) + custom elements registry -----------------------------
const shadowRoots = new WeakMap();
const customRegistry = new Map();
const customPending = new Map();

class ShadowRoot extends DocumentFragment {
  constructor(host, mode) {
    super();
    Object.defineProperty(this, 'host', { value: host });
    this.mode = mode === 'closed' ? 'closed' : 'open';
  }
  get activeElement() { return null; }
}

function upgradeCustom(el) {
  if (!el || el.nodeType !== 1) return;
  const name = (el.localName || '').toLowerCase();
  const ctor = customRegistry.get(name);
  if (ctor && Object.getPrototypeOf(el) !== ctor.prototype) {
    Object.setPrototypeOf(el, ctor.prototype);
    try { if (el.connectedCallback && el.isConnected) el.connectedCallback(); } catch (e) { reportError(e); }
  }
  // Upgrade descendants created via innerHTML/parser.
  try {
    for (const d of el.querySelectorAll('*')) {
      const n = (d.localName || '').toLowerCase();
      const c = customRegistry.get(n);
      if (c && Object.getPrototypeOf(d) !== c.prototype) Object.setPrototypeOf(d, c.prototype);
    }
  } catch (_) {}
}
const origAppendChild = Node.prototype.appendChild;
Node.prototype.appendChild = function (c) {
  const r = origAppendChild.call(this, c);
  try { upgradeCustom(c); if (c.connectedCallback && c.isConnected) c.connectedCallback(); } catch (e) { reportError(e); }
  return r;
};
const origInsertBefore = Node.prototype.insertBefore;
Node.prototype.insertBefore = function (c, ref) {
  const r = origInsertBefore.call(this, c, ref);
  try { upgradeCustom(c); if (c.connectedCallback && c.isConnected) c.connectedCallback(); } catch (e) { reportError(e); }
  return r;
};
const origRemoveChild = Node.prototype.removeChild;
Node.prototype.removeChild = function (c) {
  try { if (c.disconnectedCallback) c.disconnectedCallback(); } catch (e) { reportError(e); }
  return origRemoveChild.call(this, c);
};

const customElementsImpl = {
  define(name, ctor, opts) {
    name = String(name).toLowerCase();
    if (!/^[a-z][a-z0-9._-]*-[a-z0-9._-]+$/.test(name)) throw new DOMException(`'${name}' is not a valid custom element name`, 'SyntaxError');
    if (customRegistry.has(name)) throw new DOMException(`'${name}' has already been defined`, 'NotSupportedError');
    customRegistry.set(name, ctor);
    const pend = customPending.get(name);
    if (pend) { customPending.delete(name); for (const res of pend) try { res(ctor); } catch (_) {} }
  },
  get(name) { return customRegistry.get(String(name).toLowerCase()); },
  whenDefined(name) {
    name = String(name).toLowerCase();
    if (customRegistry.has(name)) return Promise.resolve(customRegistry.get(name));
    return new Promise(res => {
      if (!customPending.has(name)) customPending.set(name, []);
      customPending.get(name).push(res);
    });
  },
  upgrade(root) {
    try {
      const all = root.querySelectorAll ? [...root.querySelectorAll('*')] : [];
      if (root.nodeType === 1) all.unshift(root);
      for (const el of all) upgradeCustom(el);
    } catch (_) {}
  },
};

// ----- style / classList / dataset ------------------------------------------------------------

function splitDecls(text) {
  const out = []; let depth = 0, quote = null, cur = '';
  for (const ch of text) {
    if (quote) { cur += ch; if (ch === quote) quote = null; continue; }
    if (ch === '"' || ch === "'") { quote = ch; cur += ch; continue; }
    if (ch === '(') depth++; else if (ch === ')') depth--;
    if (ch === ';' && depth === 0) { out.push(cur); cur = ''; } else cur += ch;
  }
  if (cur.trim()) out.push(cur);
  return out;
}
function parseDecls(text) {
  const m = new Map();
  for (const part of splitDecls(text)) {
    const i = part.indexOf(':'); if (i < 0) continue;
    let name = part.slice(0, i).trim(); let value = part.slice(i + 1).trim(); let important = false;
    if (/!important\s*$/i.test(value)) { important = true; value = value.replace(/!important\s*$/i, '').trim(); }
    if (!name || !value) continue;
    if (!name.startsWith('--')) name = name.toLowerCase();
    m.set(name, [value, important]);
  }
  return m;
}
const serializeDecls = m => [...m].map(([k, [v, imp]]) => `${k}: ${v}${imp ? ' !important' : ''};`).join(' ');

class CSSStyleDeclaration {
  constructor(el) { Object.defineProperty(this, '_el', { value: el }); }
  _map() { return parseDecls(this._el.getAttribute('style') || ''); }
  _write(m) { const t = serializeDecls(m); if (t) this._el.setAttribute('style', t); else this._el.removeAttribute('style'); }
  getPropertyValue(n) { const e = this._map().get(String(n).startsWith('--') ? n : String(n).toLowerCase()); return e ? e[0] : ''; }
  getPropertyPriority(n) { const e = this._map().get(String(n).toLowerCase()); return e && e[1] ? 'important' : ''; }
  setProperty(n, v, prio) {
    const m = this._map(); const name = String(n).startsWith('--') ? String(n) : String(n).toLowerCase();
    if (v === null || v === undefined || v === '') m.delete(name); else m.set(name, [String(v), prio === 'important']);
    this._write(m);
  }
  removeProperty(n) { const old = this.getPropertyValue(n); const m = this._map(); m.delete(String(n).toLowerCase()); this._write(m); return old; }
  get cssText() { return serializeDecls(this._map()); }
  set cssText(v) { if (String(v).trim()) this._el.setAttribute('style', String(v)); else this._el.removeAttribute('style'); }
  get length() { return this._map().size; }
  item(i) { return [...this._map().keys()][i] || ''; }
}
const styleProxies = new WeakMap();
function styleOf(el) {
  let p = styleProxies.get(el);
  if (!p) {
    const decl = new CSSStyleDeclaration(el);
    p = new Proxy(decl, {
      get(t, k) { if (typeof k === 'symbol' || k in t) return Reflect.get(t, k, t); return t.getPropertyValue(k === 'cssFloat' ? 'float' : kebab(k)); },
      set(t, k, v) { if (k in t) { t[k] = v; return true; } t.setProperty(k === 'cssFloat' ? 'float' : kebab(String(k)), v == null ? '' : String(v)); return true; },
    });
    styleProxies.set(el, p);
  }
  return p;
}

class DOMTokenList {
  constructor(el, attr) { Object.defineProperty(this, '_el', { value: el }); Object.defineProperty(this, '_attr', { value: attr }); }
  _tokens() { return (this._el.getAttribute(this._attr) || '').split(/\s+/).filter(Boolean); }
  _set(t) { this._el.setAttribute(this._attr, t.join(' ')); }
  get length() { return this._tokens().length; }
  get value() { return this._el.getAttribute(this._attr) || ''; }
  set value(v) { this._el.setAttribute(this._attr, v); }
  item(i) { return this._tokens()[i] ?? null; }
  contains(t) { return this._tokens().includes(String(t)); }
  add(...ts) { const cur = this._tokens(); let changed = false; for (const t of ts) { if (!cur.includes(String(t))) { cur.push(String(t)); changed = true; } } if (changed) this._set(cur); }
  remove(...ts) { const cur = this._tokens(); const next = cur.filter(t => !ts.map(String).includes(t)); if (next.length !== cur.length) this._set(next); }
  toggle(t, force) {
    t = String(t); const has = this.contains(t); const want = force === undefined ? !has : !!force;
    if (want && !has) this.add(t); else if (!want && has) this.remove(t);
    return want;
  }
  replace(a, b) { const cur = this._tokens(); const i = cur.indexOf(String(a)); if (i < 0) return false; cur[i] = String(b); this._set([...new Set(cur)]); return true; }
  forEach(fn, thisArg) { this._tokens().forEach((t, i) => fn.call(thisArg, t, i, this)); }
  [Symbol.iterator]() { return this._tokens()[Symbol.iterator](); }
  entries() { return this._tokens().entries(); }
  keys() { return this._tokens().keys(); }
  values() { return this._tokens().values(); }
  toString() { return this.value; }
}

function datasetOf(el) {
  return new Proxy({}, {
    get(_, k) { if (typeof k === 'symbol') return undefined; const v = el.getAttribute('data-' + kebab(k)); return v === null ? undefined : v; },
    set(_, k, v) { el.setAttribute('data-' + kebab(String(k)), String(v)); return true; },
    deleteProperty(_, k) { el.removeAttribute('data-' + kebab(String(k))); return true; },
    has(_, k) { return typeof k === 'string' && el.hasAttribute('data-' + kebab(k)); },
    ownKeys() { return el.getAttributeNames().filter(n => n.startsWith('data-')).map(n => camel(n.slice(5))); },
    getOwnPropertyDescriptor(_, k) { const v = el.getAttribute('data-' + kebab(String(k))); return v === null ? undefined : { value: v, enumerable: true, configurable: true, writable: true }; },
  });
}

class DOMRect {
  constructor(x = 0, y = 0, width = 0, height = 0) { this.x = x; this.y = y; this.width = width; this.height = height; }
  get left() { return Math.min(this.x, this.x + this.width); } get right() { return Math.max(this.x, this.x + this.width); }
  get top() { return Math.min(this.y, this.y + this.height); } get bottom() { return Math.max(this.y, this.y + this.height); }
  toJSON() { const { x, y, width, height, top, right, bottom, left } = this; return { x, y, width, height, top, right, bottom, left }; }
  static fromRect(r = {}) { return new DOMRect(r.x, r.y, r.width, r.height); }
}

// ----- Element --------------------------------------------------------------------------------

const attrsObject = el => {
  const flat = N('attrs', el[ID]); const out = [];
  for (let i = 0; i < flat.length; i += 2) out.push({ name: flat[i], value: flat[i + 1], localName: flat[i], nodeName: flat[i], nodeValue: flat[i + 1], ownerElement: el });
  return out;
};

class Element extends Node {
  get tagName() { return (N('tag', this[ID]) || '').toUpperCase(); }
  get localName() { return N('tag', this[ID]); }
  get namespaceURI() { return 'http://www.w3.org/1999/xhtml'; }
  get id() { return this.getAttribute('id') || ''; }
  set id(v) { this.setAttribute('id', v); }
  get className() { return this.getAttribute('class') || ''; }
  set className(v) { this.setAttribute('class', v); }
  get classList() { return new DOMTokenList(this, 'class'); }
  set classList(v) { this.setAttribute('class', String(v)); }
  get attributes() { return attrsObject(this); }
  get style() { return styleOf(this); }
  set style(v) { this.setAttribute('style', String(v)); }
  get dataset() { return datasetOf(this); }
  getAttribute(n) { return N('get_attr', this[ID], String(n)); }
  setAttribute(n, v) {
    n = String(n); v = String(v);
    N('set_attr', this[ID], n, v);
    if (n.toLowerCase() === 'src' && this.localName === 'img') scheduleImageEvents(this);
  }
  removeAttribute(n) { N('remove_attr', this[ID], String(n)); }
  hasAttribute(n) { return N('get_attr', this[ID], String(n)) !== null; }
  toggleAttribute(n, force) { const has = this.hasAttribute(n); const want = force === undefined ? !has : !!force; if (want && !has) this.setAttribute(n, ''); else if (!want && has) this.removeAttribute(n); return want; }
  getAttributeNames() { const f = N('attrs', this[ID]); const out = []; for (let i = 0; i < f.length; i += 2) out.push(f[i]); return out; }
  hasAttributes() { return N('attrs', this[ID]).length > 0; }
  querySelector(s) { return wrap(mut('query', this[ID], String(s))); }
  matches(s) { return mut('matches', this[ID], String(s)); }
  webkitMatchesSelector(s) { return this.matches(s); }
  closest(s) { return wrap(mut('closest', this[ID], String(s))); }
  getElementsByTagName(t) { return HTMLCollection.from(wrapAll(N('by_tag', this[ID], String(t)))); }
  getElementsByClassName(c) { return HTMLCollection.from(wrapAll(N('by_class', this[ID], String(c)))); }
  get innerHTML() { return N('inner_html', this[ID]); }
  set innerHTML(v) { N('set_inner_html', this[ID], v == null ? '' : String(v)); }
  get outerHTML() { return N('outer_html', this[ID]); }
  set outerHTML(v) { const p = this.parentNode; if (!p) return; const f = wrap(N('parse_fragment', String(v))); p.replaceChild(f, this); }
  get innerText() { return this.textContent; }
  set innerText(v) { this.textContent = v; }
  insertAdjacentHTML(pos, html) { this.insertAdjacentElement_(pos, wrap(N('parse_fragment', String(html)))); }
  insertAdjacentElement(pos, el) { return this.insertAdjacentElement_(pos, el); }
  insertAdjacentText(pos, t) { this.insertAdjacentElement_(pos, document.createTextNode(String(t))); }
  insertAdjacentElement_(pos, node) {
    switch (String(pos).toLowerCase()) {
      case 'beforebegin': this.parentNode && this.parentNode.insertBefore(node, this); break;
      case 'afterbegin': this.insertBefore(node, this.firstChild); break;
      case 'beforeend': this.appendChild(node); break;
      case 'afterend': this.parentNode && this.parentNode.insertBefore(node, this.nextSibling); break;
      default: throw new DOMException('Invalid position', 'SyntaxError');
    }
    return node;
  }
  get nextElementSibling() { let n = this.nextSibling; while (n && n.nodeType !== 1) n = n.nextSibling; return n; }
  get previousElementSibling() { let n = this.previousSibling; while (n && n.nodeType !== 1) n = n.previousSibling; return n; }
  // geometry
  getBoundingClientRect() { const r = N('rect', this[ID]); return r ? new DOMRect(r[0], r[1], r[2], r[3]) : new DOMRect(); }
  getClientRects() { const r = this.getBoundingClientRect(); return r.width || r.height ? [r] : []; }
  get offsetWidth() { const s = N('layout_size', this[ID]); return s ? Math.round(s[0]) : 0; }
  get offsetHeight() { const s = N('layout_size', this[ID]); return s ? Math.round(s[1]) : 0; }
  get offsetLeft() { const r = N('rect', this[ID]); const p = this.offsetParent; const pr = p && p !== document.body ? N('rect', p[ID]) : null; return r ? Math.round(r[0] - (pr ? pr[0] : 0)) : 0; }
  get offsetTop() { const r = N('rect', this[ID]); const p = this.offsetParent; const pr = p && p !== document.body ? N('rect', p[ID]) : null; return r ? Math.round(r[1] - (pr ? pr[1] : 0)) : 0; }
  get offsetParent() { let p = this.parentElement; while (p) { if (p === document.body || N('computed', p[ID], 'position') !== 'static') return p; p = p.parentElement; } return null; }
  get clientWidth() { const m = N('scroll_metrics', this[ID]); return m ? Math.round(m[2]) : 0; }
  get clientHeight() { const m = N('scroll_metrics', this[ID]); return m ? Math.round(m[3]) : 0; }
  get clientLeft() { return parseFloat(N('computed', this[ID], 'border-left-width')) || 0; }
  get clientTop() { return parseFloat(N('computed', this[ID], 'border-top-width')) || 0; }
  get scrollWidth() { const m = N('scroll_metrics', this[ID]); return m ? Math.round(m[0]) : 0; }
  get scrollHeight() { const m = N('scroll_metrics', this[ID]); return m ? Math.round(m[1]) : 0; }
  get scrollTop() { return N('get_scroll', this[ID])[1]; }
  set scrollTop(v) { N('set_scroll', this[ID], undefined, +v || 0); }
  get scrollLeft() { return N('get_scroll', this[ID])[0]; }
  set scrollLeft(v) { N('set_scroll', this[ID], +v || 0, undefined); }
  scrollTo(x, y) { if (typeof x === 'object') { y = x.top; x = x.left; } N('set_scroll', this[ID], x, y); }
  scrollBy(x, y) { if (typeof x === 'object') { y = x.top; x = x.left; } const [cx, cy] = N('get_scroll', this[ID]); N('set_scroll', this[ID], cx + (x || 0), cy + (y || 0)); }
  scrollIntoView() {
    let p = this.parentElement; const r = this.getBoundingClientRect();
    while (p) {
      const m = N('scroll_metrics', p[ID]);
      if (m && m[1] > m[3] + 0.5 && /auto|scroll/.test(N('computed', p[ID], 'overflow-y'))) {
        const pr = p.getBoundingClientRect(); const cur = p.scrollTop;
        if (r.top < pr.top) p.scrollTop = cur - (pr.top - r.top); else if (r.bottom > pr.bottom) p.scrollTop = cur + (r.bottom - pr.bottom);
        break;
      }
      p = p.parentElement;
    }
  }
  // focus
  get tabIndex() { const t = this.getAttribute('tabindex'); return t === null ? (N('is_focusable', this[ID]) ? 0 : -1) : parseInt(t, 10) || 0; }
  set tabIndex(v) { this.setAttribute('tabindex', String(v)); }
  focus() { setFocus(this); }
  blur() { if (N('focused') === this[ID]) setFocus(null); }
  click() { if (this.disabled) return; dispatchClick(this, {}); }
  get hidden() { return this.hasAttribute('hidden'); }
  set hidden(v) { this.toggleAttribute('hidden', !!v); }
  get title() { return this.getAttribute('title') || ''; }
  set title(v) { this.setAttribute('title', v); }
  get lang() { return this.getAttribute('lang') || ''; }
  set lang(v) { this.setAttribute('lang', v); }
  get dir() { return this.getAttribute('dir') || ''; }
  set dir(v) { this.setAttribute('dir', v); }
  get draggable() { return this.getAttribute('draggable') === 'true'; }
  set draggable(v) { this.setAttribute('draggable', v ? 'true' : 'false'); }
  get contentEditable() {
    const v = this.getAttribute('contenteditable');
    if (v === null) return 'inherit';
    const l = String(v).toLowerCase();
    if (l === '' || l === 'true' || l === 'plaintext-only') return l === '' ? 'true' : l;
    return 'false';
  }
  set contentEditable(v) { this.setAttribute('contenteditable', String(v)); }
  get isContentEditable() {
    let el = this;
    while (el && el.getAttribute) {
      const ce = el.getAttribute('contenteditable');
      if (ce !== null) {
        const l = String(ce).toLowerCase();
        if (l === '' || l === 'true' || l === 'plaintext-only') return true;
        if (l === 'false') return false;
      }
      el = el.parentElement;
    }
    try { return designModeValue === 'on'; } catch (_) { return false; }
  }
  setPointerCapture() {} releasePointerCapture() {} hasPointerCapture() { return false; }
  attachShadow(init) {
    if (shadowRoots.has(this)) throw new DOMException('Shadow root already attached', 'NotSupportedError');
    const mode = init && init.mode === 'closed' ? 'closed' : 'open';
    const root = new ShadowRoot(this, mode);
    shadowRoots.set(this, root);
    if (mode === 'open') this._openRoot = root;
    else this._closedRoot = root;
    return root;
  }
  get shadowRoot() { return this._openRoot || null; }
  animate() { throw new DOMException('Element.animate() is not supported; use CSS transitions and @keyframes', 'NotSupportedError'); }
  get ownerSVGElement() { return null; }
}
parentNodeMixin(Element.prototype);
childNodeMixin(Element.prototype);

class HTMLElement extends Element {}
const TAGS = {
  div: 'Div', span: 'Span', p: 'Paragraph', h1: 'Heading', h2: 'Heading', h3: 'Heading', h4: 'Heading', h5: 'Heading', h6: 'Heading', ul: 'UList', ol: 'OList', li: 'LI',
  section: '', article: '', aside: '', header: '', footer: '', main: '', nav: '', pre: 'Pre', hr: 'HR', br: 'BR', table: 'Table', tr: 'TableRow', td: 'TableCell', th: 'TableCell',
  video: 'Video', audio: 'Audio', iframe: 'IFrame', details: 'Details', dialog: 'Dialog', head: 'Head', body: 'Body', html: 'Html', title: 'Title', meta: 'Meta', template: 'Template',
};
const tagClasses = new Map();
for (const [tag, name] of Object.entries(TAGS)) {
  if (!name) continue;
  const cname = `HTML${name}Element`;
  if (!global[cname]) { const C = { [cname]: class extends HTMLElement {} }[cname]; global[cname] = C; }
  tagClasses.set(tag, global[cname]);
}

function reflectStr(P, prop, attr = prop.toLowerCase(), def = '') {
  Object.defineProperty(P, prop, { get() { const v = this.getAttribute(attr); return v === null ? def : v; }, set(v) { this.setAttribute(attr, String(v)); }, configurable: true, enumerable: true });
}
function reflectBool(P, prop, attr = prop.toLowerCase()) {
  Object.defineProperty(P, prop, { get() { return this.hasAttribute(attr); }, set(v) { this.toggleAttribute(attr, !!v); }, configurable: true, enumerable: true });
}
function reflectNum(P, prop, attr = prop.toLowerCase(), def = 0) {
  Object.defineProperty(P, prop, { get() { const v = parseFloat(this.getAttribute(attr)); return Number.isNaN(v) ? def : v; }, set(v) { this.setAttribute(attr, String(v)); }, configurable: true, enumerable: true });
}

function formOf(el) { return el.closest('form'); }

// ----- constraint validation ---------------------------------------------------------------
// Minimal spec-faithful validation shared by input/textarea/select/button + form.
// Custom messages stored per-wrapper (identity is stable via wrapperCache).
const customValidityMsg = new WeakMap();
function validityStateFor(el) {
  const custom = customValidityMsg.get(el) || '';
  if (custom) return { valid: false, valueMissing: false, typeMismatch: false, patternMismatch: false, tooLong: false, tooShort: false, rangeUnderflow: false, rangeOverflow: false, stepMismatch: false, badInput: false, customError: true };
  const tag = (el.localName || '').toLowerCase();
  const disabled = el.disabled === true;
  if (disabled) return { valid: true, valueMissing: false, typeMismatch: false, patternMismatch: false, tooLong: false, tooShort: false, rangeUnderflow: false, rangeOverflow: false, stepMismatch: false, badInput: false, customError: false };
  const required = el.hasAttribute && el.hasAttribute('required');
  if (tag === 'input') {
    const type = (el.type || 'text').toLowerCase();
    if (['hidden', 'button', 'reset', 'submit', 'image'].includes(type)) return { valid: true, valueMissing: false, typeMismatch: false, patternMismatch: false, tooLong: false, tooShort: false, rangeUnderflow: false, rangeOverflow: false, stepMismatch: false, badInput: false, customError: false };
    if (type === 'checkbox') {
      const missing = !!required && !el.checked;
      return { valid: !missing, valueMissing: missing, typeMismatch: false, patternMismatch: false, tooLong: false, tooShort: false, rangeUnderflow: false, rangeOverflow: false, stepMismatch: false, badInput: false, customError: false };
    }
    if (type === 'radio') {
      if (required) {
        const name = el.getAttribute('name');
        const form = formOf(el);
        const scope = form || (el.getRootNode ? el.getRootNode() : document);
        let group = [];
        try { group = [...scope.querySelectorAll(`input[type=radio][name="${(name || '').replace(/"/g, '\\"')}"]`)]; } catch (_) { group = [el]; }
        const anyChecked = group.some(r => r.checked);
        const missing = !anyChecked;
        return { valid: !missing, valueMissing: missing, typeMismatch: false, patternMismatch: false, tooLong: false, tooShort: false, rangeUnderflow: false, rangeOverflow: false, stepMismatch: false, badInput: false, customError: false };
      }
      return { valid: true, valueMissing: false, typeMismatch: false, patternMismatch: false, tooLong: false, tooShort: false, rangeUnderflow: false, rangeOverflow: false, stepMismatch: false, badInput: false, customError: false };
    }
    const value = el.value || '';
    if (required && value === '' && type !== 'checkbox' && type !== 'radio') return { valid: false, valueMissing: true, typeMismatch: false, patternMismatch: false, tooLong: false, tooShort: false, rangeUnderflow: false, rangeOverflow: false, stepMismatch: false, badInput: false, customError: false };
    if (value === '') return { valid: true, valueMissing: false, typeMismatch: false, patternMismatch: false, tooLong: false, tooShort: false, rangeUnderflow: false, rangeOverflow: false, stepMismatch: false, badInput: false, customError: false };
    let typeMismatch = false, badInput = false;
    if (type === 'email') {
      const multiple = el.hasAttribute('multiple');
      const parts = multiple ? value.split(',').map(s => s.trim()).filter(s => s) : [value.trim()];
      typeMismatch = parts.length === 0 || parts.some(p => !/^[^\s@]+@[^\s@]+\.[^\s@]+$/.test(p));
    } else if (type === 'url') {
      try { const u = new URL(value); if (u.protocol !== 'http:' && u.protocol !== 'https:' && u.protocol !== 'ftp:') typeMismatch = true; } catch (_) { typeMismatch = true; }
    } else if (type === 'number' || type === 'range') {
      const n = Number(value);
      if (!Number.isFinite(n)) { typeMismatch = true; badInput = true; }
    }
    let patternMismatch = false;
    const pattern = el.getAttribute('pattern');
    if (pattern !== null && ['text', 'search', 'url', 'tel', 'email', 'password'].includes(type)) {
      try { if (!new RegExp(`^(?:${pattern})$`, 'u').test(value)) patternMismatch = true; } catch (_) { /* invalid pattern never matches per spec */ patternMismatch = true; }
    }
    const maxLen = parseInt(el.getAttribute('maxlength') || '', 10);
    const minLen = parseInt(el.getAttribute('minlength') || '', 10);
    const tooLong = Number.isFinite(maxLen) && maxLen >= 0 && [...value].length > maxLen;
    const tooShort = Number.isFinite(minLen) && minLen > 0 && [...value].length < minLen;
    let under = false, over = false, stepMismatch = false;
    const min = el.getAttribute('min'), max = el.getAttribute('max');
    if ((type === 'number' || type === 'range') && !badInput) {
      const n = Number(value);
      if (min !== null && min !== '' && Number.isFinite(Number(min)) && n < Number(min)) under = true;
      if (max !== null && max !== '' && Number.isFinite(Number(max)) && n > Number(max)) over = true;
      const step = el.getAttribute('step');
      if (step !== null && step !== '' && step.toLowerCase() !== 'any') {
        const s = Number(step);
        const base = (min !== null && min !== '' && Number.isFinite(Number(min))) ? Number(min) : 0;
        if (Number.isFinite(s) && s > 0) {
          const diff = (n - base) / s;
          if (Math.abs(diff - Math.round(diff)) > 1e-9) stepMismatch = true;
        }
      }
    }
    const valid = !(typeMismatch || patternMismatch || tooLong || tooShort || under || over || stepMismatch || badInput);
    return { valid, valueMissing: false, typeMismatch, patternMismatch, tooLong, tooShort, rangeUnderflow: under, rangeOverflow: over, stepMismatch, badInput, customError: false };
  }
  if (tag === 'textarea') {
    const value = el.value || '';
    if (required && value === '') return { valid: false, valueMissing: true, typeMismatch: false, patternMismatch: false, tooLong: false, tooShort: false, rangeUnderflow: false, rangeOverflow: false, stepMismatch: false, badInput: false, customError: false };
    const maxLen = parseInt(el.getAttribute('maxlength') || '', 10);
    const minLen = parseInt(el.getAttribute('minlength') || '', 10);
    const tooLong = Number.isFinite(maxLen) && maxLen >= 0 && [...value].length > maxLen;
    const tooShort = Number.isFinite(minLen) && minLen > 0 && value !== '' && [...value].length < minLen;
    const valid = !(tooLong || tooShort);
    return { valid, valueMissing: false, typeMismatch: false, patternMismatch: false, tooLong, tooShort, rangeUnderflow: false, rangeOverflow: false, stepMismatch: false, badInput: false, customError: false };
  }
  if (tag === 'select') {
    if (required) {
      const v = el.value || '';
      // A placeholder label option (empty value) fails required.
      const missing = v === '';
      return { valid: !missing, valueMissing: missing, typeMismatch: false, patternMismatch: false, tooLong: false, tooShort: false, rangeUnderflow: false, rangeOverflow: false, stepMismatch: false, badInput: false, customError: false };
    }
    return { valid: true, valueMissing: false, typeMismatch: false, patternMismatch: false, tooLong: false, tooShort: false, rangeUnderflow: false, rangeOverflow: false, stepMismatch: false, badInput: false, customError: false };
  }
  return { valid: true, valueMissing: false, typeMismatch: false, patternMismatch: false, tooLong: false, tooShort: false, rangeUnderflow: false, rangeOverflow: false, stepMismatch: false, badInput: false, customError: false };
}
function checkValidityOf(el) {
  const v = validityStateFor(el);
  if (v.valid) return true;
  try { el.dispatchEvent(new Event('invalid', { cancelable: true, bubbles: false })); } catch (_) {}
  return false;
}
function reportValidityOf(el) {
  const ok = checkValidityOf(el);
  if (!ok) { try { if (el.focus) el.focus(); } catch (_) {} }
  return ok;
}

// Picker UIs. File inputs use the real OS dialog; select/date render an in-engine popup
// (there is no OS-level list/calendar widget in the native runtime).
function fileNameOf(p) { return String(p).replace(/\\/g, '/').split('/').pop() || 'file'; }
function dismissOnOutside(popup) {
  setTimeout(() => {
    const close = e => {
      if (!popup.contains(e.target)) {
        popup.remove();
        document.removeEventListener('pointerdown', close, true);
      }
    };
    document.addEventListener('pointerdown', close, true);
  }, 0);
}
function openFilePicker(input) {
  const accept = input.getAttribute('accept') || '';
  const exts = accept.split(',').map(s => s.trim().replace(/^\./, '')).filter(s => s && !s.includes('/'));
  const opts = { title: 'Choose a file', multiple: input.hasAttribute('multiple'), properties: ['openFile'] };
  if (exts.length) opts.filters = [{ name: 'Accepted files', extensions: exts }];
  nativeDialog(false, opts).then(paths => {
    if (!paths || !paths.length) return;
    const files = paths.map(p => new File([], fileNameOf(p), { path: p }));
    input.files = files;
    if (files[0]) input.value = files[0].path;
    input.dispatchEvent(new Event('input', { bubbles: true }));
    input.dispatchEvent(new Event('change', { bubbles: true }));
  }).catch(() => {});
}
function openDatePicker(input) {
  const r = input.getBoundingClientRect();
  let year, month;
  if (/^\d{4}-\d{2}-\d{2}$/.test(input.value)) {
    const d = new Date(input.value + 'T00:00:00'); year = d.getFullYear(); month = d.getMonth();
  } else { const d = new Date(); year = d.getFullYear(); month = d.getMonth(); }
  const popup = document.createElement('div');
  Object.assign(popup.style, { position: 'fixed', left: r.left + 'px', top: r.bottom + 'px', zIndex: '10000',
    background: '#fff', border: '1px solid #888', padding: '6px', boxShadow: '0 2px 8px #0004', font: '12px sans-serif', color: '#000' });
  const title = document.createElement('div');
  const grid = document.createElement('div');
  Object.assign(grid.style, { display: 'grid', gridTemplateColumns: 'repeat(7, 24px)', gap: '2px' });
  const render = () => {
    title.textContent = year + '-' + String(month + 1).padStart(2, '0');
    grid.textContent = '';
    for (const [label, step] of [['<', -1], ['>', 1]]) {
      const b = document.createElement('button'); b.textContent = label;
      b.onclick = () => { month += step; if (month < 0) { month = 11; year--; } if (month > 11) { month = 0; year++; } render(); };
      grid.appendChild(b);
    }
    grid.appendChild(document.createElement('span'));
    const first = new Date(year, month, 1).getDay();
    for (let i = 0; i < first; i++) grid.appendChild(document.createElement('span'));
    const days = new Date(year, month + 1, 0).getDate();
    for (let d = 1; d <= days; d++) {
      const b = document.createElement('button'); b.textContent = String(d);
      b.onclick = () => {
        input.value = `${year}-${String(month + 1).padStart(2, '0')}-${String(d).padStart(2, '0')}`;
        input.dispatchEvent(new Event('input', { bubbles: true }));
        input.dispatchEvent(new Event('change', { bubbles: true }));
        popup.remove();
      };
      grid.appendChild(b);
    }
  };
  popup.appendChild(title); popup.appendChild(grid); render();
  document.body.appendChild(popup);
  dismissOnOutside(popup);
}
function openSelectPicker(select) {
  const r = select.getBoundingClientRect();
  const opts = [...select.options];
  if (!opts.length) return;
  const popup = document.createElement('div');
  Object.assign(popup.style, { position: 'fixed', left: r.left + 'px', top: r.bottom + 'px', minWidth: Math.max(r.width, 60) + 'px',
    zIndex: '10000', background: '#fff', border: '1px solid #888', boxShadow: '0 2px 8px #0004', font: '13px sans-serif', color: '#000' });
  const current = select.selectedIndex;
  opts.forEach((o, i) => {
    const item = document.createElement('div');
    item.textContent = o.textContent;
    Object.assign(item.style, { padding: '3px 8px', cursor: 'pointer', background: i === current ? '#06f' : 'transparent', color: i === current ? '#fff' : '#000' });
    item.onclick = () => {
      select.selectedIndex = i;
      select.dispatchEvent(new Event('input', { bubbles: true }));
      select.dispatchEvent(new Event('change', { bubbles: true }));
      popup.remove();
    };
    popup.appendChild(item);
  });
  document.body.appendChild(popup);
  dismissOnOutside(popup);
}

class HTMLInputElement extends HTMLElement {
  get value() { return N('value', this[ID]); }
  set value(v) { N('set_value', this[ID], v == null ? '' : String(v)); }
  get checked() { return N('checked', this[ID]); }
  set checked(v) { N('set_checked', this[ID], !!v); }
  get defaultValue() { return this.getAttribute('value') || ''; }
  set defaultValue(v) { this.setAttribute('value', v); }
  get defaultChecked() { return this.hasAttribute('checked'); }
  get type() { return (this.getAttribute('type') || 'text').toLowerCase(); }
  set type(v) { this.setAttribute('type', v); }
  get valueAsNumber() { const n = parseFloat(this.value); return Number.isNaN(n) ? NaN : n; }
  set valueAsNumber(v) { this.value = String(v); }
  get valueAsDate() {
    const v = this.value;
    if (!v) return null;
    const d = new Date(v.length === 10 ? v + 'T00:00:00' : v);
    return Number.isNaN(d.getTime()) ? null : d;
  }
  set valueAsDate(d) {
    if (d === null || d === undefined) { this.value = ''; return; }
    const t = (this.type || '').toLowerCase();
    const dt = d instanceof Date ? d : new Date(d);
    if (Number.isNaN(dt.getTime())) { this.value = ''; return; }
    const pad = n => String(n).padStart(2, '0');
    if (t === 'date') this.value = `${dt.getFullYear()}-${pad(dt.getMonth() + 1)}-${pad(dt.getDate())}`;
    else if (t === 'month') this.value = `${dt.getFullYear()}-${pad(dt.getMonth() + 1)}`;
    else if (t === 'time') this.value = `${pad(dt.getHours())}:${pad(dt.getMinutes())}`;
    else this.value = dt.toISOString();
  }
  stepUp(n = 1) { this._stepBy(+n || 0); }
  stepDown(n = 1) { this._stepBy(-(+n || 0)); }
  _stepBy(delta) {
    const t = (this.type || '').toLowerCase();
    if (!['number', 'range'].includes(t)) return;
    const cur = Number(this.value);
    const base = Number.isFinite(cur) ? cur : 0;
    const stepAttr = this.getAttribute('step');
    let step = 1;
    if (stepAttr !== null && stepAttr !== '' && stepAttr.toLowerCase() !== 'any') {
      const s = Number(stepAttr);
      if (Number.isFinite(s) && s > 0) step = s;
    }
    const next = base + step * delta;
    const min = this.getAttribute('min'), max = this.getAttribute('max');
    let v = next;
    if (min !== null && min !== '' && Number.isFinite(Number(min))) v = Math.max(v, Number(min));
    if (max !== null && max !== '' && Number.isFinite(Number(max))) v = Math.min(v, Number(max));
    this.value = String(v);
    try { this.dispatchEvent(new Event('input', { bubbles: true })); } catch (_) {}
    try { this.dispatchEvent(new Event('change', { bubbles: true })); } catch (_) {}
  }
  showPicker() {
    const t = (this.type || '').toLowerCase();
    if (t === 'checkbox' || t === 'radio') { this.click(); return; }
    try { this.focus(); } catch (_) {}
    if (t === 'file') { openFilePicker(this); return; }
    if (t === 'date') { openDatePicker(this); return; }
    // Other input types have no picker; focusing is the honest fallback.
  }
  get selectionStart() { const s = N('selection', this[ID]); return s ? s[0] : null; }
  set selectionStart(v) { const s = N('selection', this[ID]); N('set_selection', this[ID], +v, s[1]); }
  get selectionEnd() { const s = N('selection', this[ID]); return s ? s[1] : null; }
  set selectionEnd(v) { const s = N('selection', this[ID]); N('set_selection', this[ID], s[0], +v); }
  get selectionDirection() { return 'none'; }
  setSelectionRange(a, b) { N('set_selection', this[ID], a, b); }
  select() { N('set_selection', this[ID], 0, this.value.length); }
  setRangeText(t, s, e) { const v = this.value; s = s ?? this.selectionStart; e = e ?? this.selectionEnd; this.value = v.slice(0, s) + t + v.slice(e); }
  get form() { return formOf(this); }
  get files() { return this._files || []; }
  set files(v) { this._files = v; }
  get labels() { return NodeList.from([]); }
  get validity() { return validityStateFor(this); }
  get validationMessage() { return customValidityMsg.get(this) || ''; }
  get willValidate() { const t = (this.type || '').toLowerCase(); return !this.disabled && !['hidden', 'button', 'reset', 'submit', 'image'].includes(t); }
  checkValidity() { return checkValidityOf(this); }
  reportValidity() { return reportValidityOf(this); }
  setCustomValidity(m) { customValidityMsg.set(this, String(m || '')); }
  get indeterminate() { return false; } set indeterminate(v) {}
}
for (const p of ['name', 'placeholder', 'autocomplete', 'min', 'max', 'step', 'pattern', 'accept', 'alt', 'src']) reflectStr(HTMLInputElement.prototype, p);
for (const p of ['disabled', 'required', 'readOnly', 'multiple', 'autofocus']) reflectBool(HTMLInputElement.prototype, p);
reflectNum(HTMLInputElement.prototype, 'maxLength', 'maxlength', -1); reflectNum(HTMLInputElement.prototype, 'size', 'size', 20);

class HTMLTextAreaElement extends HTMLElement {
  get value() { return N('value', this[ID]); }
  set value(v) { N('set_value', this[ID], v == null ? '' : String(v)); }
  get defaultValue() { return this.textContent; }
  get type() { return 'textarea'; }
  get textLength() { return this.value.length; }
  get selectionStart() { return N('selection', this[ID])[0]; }
  set selectionStart(v) { N('set_selection', this[ID], +v, N('selection', this[ID])[1]); }
  get selectionEnd() { return N('selection', this[ID])[1]; }
  set selectionEnd(v) { N('set_selection', this[ID], N('selection', this[ID])[0], +v); }
  setSelectionRange(a, b) { N('set_selection', this[ID], a, b); }
  select() { N('set_selection', this[ID], 0, this.value.length); }
  get form() { return formOf(this); }
  get validity() { return validityStateFor(this); }
  get validationMessage() { return customValidityMsg.get(this) || ''; }
  get willValidate() { return !this.disabled; }
  checkValidity() { return checkValidityOf(this); }
  reportValidity() { return reportValidityOf(this); }
  setCustomValidity(m) { customValidityMsg.set(this, String(m || '')); }
}
for (const p of ['name', 'placeholder', 'wrap']) reflectStr(HTMLTextAreaElement.prototype, p);
for (const p of ['disabled', 'required', 'readOnly', 'autofocus']) reflectBool(HTMLTextAreaElement.prototype, p);
reflectNum(HTMLTextAreaElement.prototype, 'rows', 'rows', 2); reflectNum(HTMLTextAreaElement.prototype, 'cols', 'cols', 20); reflectNum(HTMLTextAreaElement.prototype, 'maxLength', 'maxlength', -1);

class HTMLOptionElement extends HTMLElement {
  get value() { const v = this.getAttribute('value'); return v === null ? this.textContent.trim() : v; }
  set value(v) { this.setAttribute('value', v); }
  get text() { return this.textContent.trim(); }
  get label() { return this.getAttribute('label') || this.text; }
  get selected() { return this.hasAttribute('selected'); }
  set selected(v) { const sel = this.closest('select'); if (v && sel) for (const o of sel.options) o.toggleAttribute('selected', o === this); else this.toggleAttribute('selected', !!v); }
  get index() { const s = this.closest('select'); return s ? s.options.indexOf(this) : 0; }
}
reflectBool(HTMLOptionElement.prototype, 'disabled');

class HTMLSelectElement extends HTMLElement {
  get options() { return HTMLCollection.from(this.querySelectorAll('option')); }
  get value() { const o = this.options[this.selectedIndex]; return o ? o.value : ''; }
  set value(v) { const opts = this.options; const i = opts.findIndex(o => o.value === String(v)); opts.forEach((o, k) => o.toggleAttribute('selected', k === i)); }
  get selectedIndex() { const opts = this.options; const i = opts.findIndex(o => o.hasAttribute('selected')); return opts.length ? Math.max(i, 0) : -1; }
  set selectedIndex(i) { this.options.forEach((o, k) => o.toggleAttribute('selected', k === i)); }
  get selectedOptions() { return HTMLCollection.from(this.options.filter((o, i) => i === this.selectedIndex)); }
  get length() { return this.options.length; }
  get type() { return 'select-one'; }
  get form() { return formOf(this); }
  add(o, before) { this.insertBefore(o, before || null); }
  showPicker() {
    // Render an in-engine popup list anchored to the select. Keyboard arrows/Home/End
    // still work directly on the focused control (see the runtime input driver).
    openSelectPicker(this);
  }
  get validity() { return validityStateFor(this); }
  get validationMessage() { return customValidityMsg.get(this) || ''; }
  get willValidate() { return !this.disabled; }
  checkValidity() { return checkValidityOf(this); }
  reportValidity() { return reportValidityOf(this); }
  setCustomValidity(m) { customValidityMsg.set(this, String(m || '')); }
}
for (const p of ['name']) reflectStr(HTMLSelectElement.prototype, p);
for (const p of ['disabled', 'required', 'multiple']) reflectBool(HTMLSelectElement.prototype, p);

class HTMLButtonElement extends HTMLElement {
  get type() { const t = (this.getAttribute('type') || 'submit').toLowerCase(); return ['button', 'reset', 'submit'].includes(t) ? t : 'submit'; }
  set type(v) { this.setAttribute('type', v); }
  get form() { return formOf(this); }
  get value() { return this.getAttribute('value') || ''; }
  set value(v) { this.setAttribute('value', v); }
  get validity() { return { valid: true, valueMissing: false, typeMismatch: false, patternMismatch: false, tooLong: false, tooShort: false, rangeUnderflow: false, rangeOverflow: false, stepMismatch: false, badInput: false, customError: false }; }
  get validationMessage() { return ''; }
  get willValidate() { return false; }
  checkValidity() { return true; }
  reportValidity() { return true; }
  setCustomValidity() {}
}
reflectStr(HTMLButtonElement.prototype, 'name'); reflectBool(HTMLButtonElement.prototype, 'disabled'); reflectBool(HTMLButtonElement.prototype, 'autofocus');

class HTMLFormElement extends HTMLElement {
  get elements() { return HTMLCollection.from(this.querySelectorAll('input, select, textarea, button')); }
  get length() { return this.elements.length; }
  submit() { /* no navigation in Rendrix */ }
  requestSubmit(submitter) {
    if (!this.hasAttribute('novalidate') && !this.checkValidity()) return;
    const ev = new SubmitEvent('submit', { bubbles: true, cancelable: true, submitter: submitter || null });
    this.dispatchEvent(ev);
  }
  reset() {
    const ev = new Event('reset', { bubbles: true, cancelable: true });
    if (!this.dispatchEvent(ev)) return;
    for (const c of this.elements) {
      if (c.localName === 'input') { if (c.type === 'checkbox' || c.type === 'radio') c.checked = c.defaultChecked; else c.value = c.defaultValue; }
      else if (c.localName === 'textarea') c.value = c.defaultValue;
    }
  }
  checkValidity() {
    if (this.hasAttribute('novalidate')) return true;
    let ok = true;
    for (const c of this.elements) {
      if (c.checkValidity && !c.checkValidity()) ok = false;
    }
    return ok;
  }
  reportValidity() {
    const ok = this.checkValidity();
    if (!ok) {
      for (const c of this.elements) {
        if (c.validity && !c.validity.valid) { try { c.focus(); } catch (_) {} break; }
      }
    }
    return ok;
  }
}
reflectStr(HTMLFormElement.prototype, 'action'); reflectStr(HTMLFormElement.prototype, 'method', 'method', 'get'); reflectStr(HTMLFormElement.prototype, 'name');

class HTMLAnchorElement extends HTMLElement {
  get href() { const h = this.getAttribute('href'); if (h === null) return ''; try { return new URL(h, location.href).href; } catch (_) { return h; } }
  set href(v) { this.setAttribute('href', v); }
  get pathname() { try { return new URL(this.href).pathname; } catch (_) { return ''; } }
  get search() { try { return new URL(this.href).search; } catch (_) { return ''; } }
  get hash() { try { return new URL(this.href).hash; } catch (_) { return ''; } }
  get origin() { try { return new URL(this.href).origin; } catch (_) { return ''; } }
  toString() { return this.href; }
}
for (const p of ['target', 'rel', 'download']) reflectStr(HTMLAnchorElement.prototype, p);

class HTMLLabelElement extends HTMLElement {
  get control() { const f = this.getAttribute('for'); return f ? document.getElementById(f) : this.querySelector('input, select, textarea, button'); }
  get htmlFor() { return this.getAttribute('for') || ''; }
  set htmlFor(v) { this.setAttribute('for', v); }
  get form() { return formOf(this); }
}

class HTMLImageElement extends HTMLElement {
  get src() { const s = this.getAttribute('src'); if (s === null) return ''; try { return new URL(s, location.href).href; } catch (_) { return s; } }
  set src(v) { this.setAttribute('src', v); }
  get currentSrc() { return this.src; }
  get naturalWidth() { const i = N('image_info', this.getAttribute('src') || ''); return i ? i[0] : 0; }
  get naturalHeight() { const i = N('image_info', this.getAttribute('src') || ''); return i ? i[1] : 0; }
  get complete() { return !this.hasAttribute('src') || N('image_info', this.getAttribute('src')) !== null; }
  get width() { return this.hasAttribute('width') ? parseFloat(this.getAttribute('width')) : (this.offsetWidth || this.naturalWidth); }
  set width(v) { this.setAttribute('width', String(v)); }
  get height() { return this.hasAttribute('height') ? parseFloat(this.getAttribute('height')) : (this.offsetHeight || this.naturalHeight); }
  set height(v) { this.setAttribute('height', String(v)); }
  decode() { return this.complete ? Promise.resolve() : Promise.reject(new DOMException('Image failed to decode', 'EncodingError')); }
}
reflectStr(HTMLImageElement.prototype, 'alt'); reflectStr(HTMLImageElement.prototype, 'loading'); reflectStr(HTMLImageElement.prototype, 'decoding');
function scheduleImageEvents(img) {
  setTimeout(() => {
    const src = img.getAttribute('src');
    if (!src) return;
    img.dispatchEvent(new Event(N('image_info', src) ? 'load' : 'error'));
  }, 0);
}

class CanvasGradient {
  constructor(kind, p) { this._kind = kind; Object.assign(this, p); this._stops = []; }
  addColorStop(offset, color) {
    offset = +offset;
    if (!Number.isFinite(offset) || offset < 0 || offset > 1) throw new DOMException('The provided float value is non-finite', 'SyntaxError');
    this._stops.push([offset, String(color)]);
  }
}
const pathFinalizer = typeof FinalizationRegistry === 'function'
  ? new FinalizationRegistry(pid => { try { N('canvas_path_drop', pid); } catch (_) {} })
  : { register() {} };
class Path2D {
  constructor(path) {
    if (path === undefined) this._pid = N('canvas_path_create');
    else if (path instanceof Path2D) this._pid = mut('canvas_path_clone', path._pid);
    else throw new DOMException('Path2D string parsing is not supported', 'NotSupportedError');
    pathFinalizer.register(this, this._pid);
  }
  moveTo(x, y) { N('canvas_path_move_to', this._pid, +x, +y); }
  lineTo(x, y) { N('canvas_path_line_to', this._pid, +x, +y); }
  rect(x, y, w, h) { N('canvas_path_rect', this._pid, +x, +y, +w, +h); }
  arc(x, y, r, a0, a1, ccw) { mut('canvas_path_arc', this._pid, +x, +y, +r, +a0, +a1, !!ccw); }
  ellipse(x, y, rx, ry, rot, a0, a1, ccw) { mut('canvas_path_ellipse', this._pid, +x, +y, +rx, +ry, +rot, +a0, +a1, !!ccw); }
  closePath() { N('canvas_path_close', this._pid); }
}
class CanvasRenderingContext2D {
  constructor(canvas) {
    Object.defineProperty(this, '_id', { value: canvas[ID] });
    this._resetCache();
    N('canvas_ensure', this._id);
  }
  _resetCache() {
    this._fill = '#000000'; this._stroke = '#000000';
    this._font = '10px sans-serif'; this._align = 'start'; this._baseline = 'alphabetic';
    this._shadowColor = 'rgba(0, 0, 0, 0)'; this._shadowBlur = 0; this._shadowX = 0; this._shadowY = 0;
    this._composite = 'source-over';
    this._stack = [];
  }
  get canvas() { return wrap(this._id); }
  // ----- state -----
  save() {
    N('canvas_save', this._id);
    this._stack.push([this._fill, this._stroke, this._lineWidth, this._lineCap, this._lineJoin, this._alpha, this._composite,
      this._font, this._align, this._baseline, this._shadowColor, this._shadowBlur, this._shadowX, this._shadowY]);
  }
  restore() {
    N('canvas_restore', this._id);
    const s = this._stack.pop();
    if (s) [this._fill, this._stroke, this._lineWidth, this._lineCap, this._lineJoin, this._alpha, this._composite,
      this._font, this._align, this._baseline, this._shadowColor, this._shadowBlur, this._shadowX, this._shadowY] = s;
  }
  // ----- transform (canvas matrix convention: x' = a*x + c*y + e) -----
  translate(x, y) { N('canvas_transform', this._id, 1, 0, 0, 1, +x, +y); }
  scale(x, y) { N('canvas_transform', this._id, +x, 0, 0, +(y === undefined ? x : y), 0, 0); }
  rotate(a) { const c = Math.cos(a), s = Math.sin(a); N('canvas_transform', this._id, c, s, -s, c, 0, 0); }
  transform(a, b, c, d, e, f) { N('canvas_transform', this._id, +a, +b, +c, +d, +e, +f); }
  setTransform(a, b, c, d, e, f) {
    if (a === undefined) N('canvas_set_transform', this._id, 1, 0, 0, 1, 0, 0);
    else N('canvas_set_transform', this._id, +a, +b, +c, +d, +e, +f);
  }
  resetTransform() { N('canvas_set_transform', this._id, 1, 0, 0, 1, 0, 0); }
  // ----- styles (invalid values are ignored, per spec) -----
  _paintJSON(v) {
    if (v instanceof CanvasGradient) {
      if (!v._stops.length) return JSON.stringify({ color: '#000000' });
      if (v._kind === 'radial') return JSON.stringify({ grad: { type: 'radial', x0: v._x0, y0: v._y0, r0: v._r0, x1: v._x1, y1: v._y1, r1: v._r1, stops: v._stops } });
      if (v._kind === 'conic') return JSON.stringify({ grad: { type: 'conic', x: v._x, y: v._y, start: v._start, stops: v._stops } });
      return JSON.stringify({ grad: { type: 'linear', x0: v._x0, y0: v._y0, x1: v._x1, y1: v._y1, stops: v._stops } });
    }
    return JSON.stringify({ color: String(v) });
  }
  get fillStyle() { return this._fill; }
  set fillStyle(v) { try { N('canvas_set_fill', this._id, this._paintJSON(v)); this._fill = v; } catch (_) {} }
  get strokeStyle() { return this._stroke; }
  set strokeStyle(v) { try { N('canvas_set_stroke', this._id, this._paintJSON(v)); this._stroke = v; } catch (_) {} }
  get lineWidth() { return this._lineWidth === undefined ? 1 : this._lineWidth; }
  set lineWidth(v) { v = +v; if (Number.isFinite(v) && v > 0) { N('canvas_set_line_width', this._id, v); this._lineWidth = v; } }
  get lineCap() { return this._lineCap || 'butt'; }
  set lineCap(v) { if (['butt', 'round', 'square'].includes(v)) { N('canvas_set_line_cap', this._id, v); this._lineCap = v; } }
  get lineJoin() { return this._lineJoin || 'miter'; }
  set lineJoin(v) { if (['miter', 'round', 'bevel'].includes(v)) { N('canvas_set_line_join', this._id, v); this._lineJoin = v; } }
  get globalAlpha() { return this._alpha === undefined ? 1 : this._alpha; }
  set globalAlpha(v) { v = +v; if (Number.isFinite(v) && v >= 0 && v <= 1) { N('canvas_set_alpha', this._id, v); this._alpha = v; } }
  // Shadows: box-blur approximation; drawImage casts no shadow (documented). Invalid values ignored.
  get shadowColor() { return this._shadowColor; }
  set shadowColor(v) { try { N('canvas_set_shadow_color', this._id, String(v)); this._shadowColor = String(v); } catch (_) {} }
  get shadowBlur() { return this._shadowBlur; }
  set shadowBlur(v) { v = +v; if (Number.isFinite(v) && v >= 0) { N('canvas_set_shadow_blur', this._id, v); this._shadowBlur = v; } }
  get shadowOffsetX() { return this._shadowX; }
  set shadowOffsetX(v) { v = +v; if (Number.isFinite(v)) { N('canvas_set_shadow_offset', this._id, v, this._shadowY); this._shadowX = v; } }
  get shadowOffsetY() { return this._shadowY; }
  set shadowOffsetY(v) { v = +v; if (Number.isFinite(v)) { N('canvas_set_shadow_offset', this._id, this._shadowX, v); this._shadowY = v; } }
  // Porter-Duff and separable blend modes are honoured by the CPU rasterizer.
  // Non-separable modes (hue/saturation/color/luminosity) fall back to source-over.
  get globalCompositeOperation() { return N('canvas_get_composite', this._id); }
  set globalCompositeOperation(v) {
    const mode = String(v);
    try { N('canvas_set_composite', this._id, mode); this._composite = mode; } catch (_) {}
  }
  // ----- text (shaped through the shared TextSystem; invalid values are ignored) -----
  get font() { return this._font; }
  set font(v) { v = String(v); try { mut('canvas_set_font', this._id, v); this._font = v; } catch (_) {} }
  get textAlign() { return this._align; }
  set textAlign(v) { if (['start', 'end', 'left', 'right', 'center'].includes(v)) { N('canvas_set_text_align', this._id, v); this._align = v; } }
  get textBaseline() { return this._baseline; }
  set textBaseline(v) { if (['alphabetic', 'top', 'hanging', 'middle', 'ideographic', 'bottom'].includes(v)) { N('canvas_set_text_baseline', this._id, v); this._baseline = v; } }
  fillText(text, x, y, maxWidth) {
    if (maxWidth === undefined) N('canvas_fill_text', this._id, String(text), +x, +y);
    else N('canvas_fill_text', this._id, String(text), +x, +y, +maxWidth);
  }
  strokeText(text, x, y, maxWidth) {
    if (maxWidth === undefined) N('canvas_stroke_text', this._id, String(text), +x, +y);
    else N('canvas_stroke_text', this._id, String(text), +x, +y, +maxWidth);
  }
  measureText(text) {
    const m = JSON.parse(N('canvas_measure_text', this._id, String(text)));
    return {
      width: m.w, actualBoundingBoxLeft: 0, actualBoundingBoxRight: m.w,
      actualBoundingBoxAscent: m.a, actualBoundingBoxDescent: m.d,
      fontBoundingBoxAscent: m.a, fontBoundingBoxDescent: m.d,
      emHeightAscent: m.a, emHeightDescent: m.d, alphabeticBaseline: m.a,
    };
  }
  // ----- paths (each of fill/stroke/clip takes an optional Path2D + fillRule) -----
  _pathArgs(path, rule) {
    if (path instanceof Path2D) return [path._pid, typeof rule === 'string' ? rule : ''];
    if (typeof path === 'string') return [0, path];
    return [0, ''];
  }
  beginPath() { N('canvas_begin', this._id); }
  moveTo(x, y) { N('canvas_move_to', this._id, +x, +y); }
  lineTo(x, y) { N('canvas_line_to', this._id, +x, +y); }
  rect(x, y, w, h) { N('canvas_rect', this._id, +x, +y, +w, +h); }
  arc(x, y, r, a0, a1, ccw) { mut('canvas_arc', this._id, +x, +y, +r, +a0, +a1, !!ccw); }
  ellipse(x, y, rx, ry, rot, a0, a1, ccw) { mut('canvas_ellipse', this._id, +x, +y, +rx, +ry, +rot, +a0, +a1, !!ccw); }
  closePath() { N('canvas_close', this._id); }
  fill(path, rule) { const [pid, r] = this._pathArgs(path, rule); mut('canvas_fill', this._id, pid, r); }
  stroke(path) { mut('canvas_stroke', this._id, path instanceof Path2D ? path._pid : 0); }
  clip(path, rule) { const [pid, r] = this._pathArgs(path, rule); mut('canvas_clip', this._id, pid, r); }
  fillRect(x, y, w, h) { N('canvas_fill_rect', this._id, +x, +y, +w, +h); }
  strokeRect(x, y, w, h) { N('canvas_stroke_rect', this._id, +x, +y, +w, +h); }
  clearRect(x, y, w, h) { N('canvas_clear_rect', this._id, +x, +y, +w, +h); }
  createLinearGradient(x0, y0, x1, y1) { return new CanvasGradient('linear', { _x0: +x0, _y0: +y0, _x1: +x1, _y1: +y1 }); }
  createRadialGradient(x0, y0, r0, x1, y1, r1) {
    [x0, y0, r0, x1, y1, r1] = [+x0, +y0, +r0, +x1, +y1, +r1];
    if (r0 < 0 || r1 < 0) throw new DOMException('The radius is negative', 'IndexSizeError');
    if (![x0, y0, r0, x1, y1, r1].every(Number.isFinite)) throw new DOMException('The provided float value is non-finite', 'SyntaxError');
    return new CanvasGradient('radial', { _x0: x0, _y0: y0, _r0: r0, _x1: x1, _y1: y1, _r1: r1 });
  }
  createConicGradient(startAngle, x, y) {
    [startAngle, x, y] = [+startAngle, +x, +y];
    if (![startAngle, x, y].every(Number.isFinite)) throw new DOMException('The provided float value is non-finite', 'SyntaxError');
    return new CanvasGradient('conic', { _start: startAngle, _x: x, _y: y });
  }
  createPattern() { throw new DOMException('Patterns are not supported', 'NotSupportedError'); }
  // ----- images -----
  drawImage(src, ...a) {
    let sj, sx, sy, sw, sh, dx, dy, dw, dh;
    if (src instanceof HTMLCanvasElement || src instanceof ImageBitmap) {
      sj = JSON.stringify({ canvas: src[ID] }); sw = src.width; sh = src.height;
    } else if (src instanceof HTMLImageElement) {
      const info = N('image_info', src.getAttribute('src') || '');
      if (!info) return; // not loaded yet: paints nothing
      sj = JSON.stringify({ img: src.getAttribute('src') }); sw = info[0]; sh = info[1];
    } else throw new TypeError("drawImage: argument is not an image or canvas");
    if (a.length === 2) [dx, dy] = a, [dw, dh] = [sw, sh], [sx, sy] = [0, 0];
    else if (a.length === 4) [dx, dy, dw, dh] = a, [sx, sy] = [0, 0];
    else if (a.length === 8) [sx, sy, sw, sh, dx, dy, dw, dh] = a;
    else throw new TypeError(`drawImage: ${a.length + 1} arguments`);
    if (sw <= 0 || sh <= 0 || dw === 0 || dh === 0) return;
    mut('canvas_draw_image', this._id, sj, +sx, +sy, +sw, +sh, +dx, +dy, +dw, +dh);
  }
  // ----- pixels (get/put round-trip raw bytes; large rects base64-encode the whole area —
  // prefer small rects, see the perf caveat in the A3 report) -----
  getImageData(sx, sy, sw, sh) {
    sw = Math.floor(sw); sh = Math.floor(sh);
    if (sw === 0 || sh === 0) throw new DOMException('getImageData dimensions must be non-zero', 'IndexSizeError');
    const b64 = N('canvas_get_data', this._id, Math.floor(sx), Math.floor(sy), sw, sh);
    return new ImageData(new Uint8ClampedArray(b64ToBytes(b64).buffer), sw, sh);
  }
  putImageData(data, dx, dy) {
    if (!(data instanceof ImageData)) throw new TypeError('putImageData: argument is not ImageData');
    N('canvas_put_data', this._id, bytesToB64(data.data), data.width, data.height, Math.floor(dx), Math.floor(dy));
  }
  createImageData(a, b) {
    if (a instanceof ImageData) return new ImageData(new Uint8ClampedArray(a.data), a.width, a.height);
    const w = Math.floor(a), h = Math.floor(b);
    if (w === 0 || h === 0) throw new DOMException('createImageData dimensions must be non-zero', 'IndexSizeError');
    return new ImageData(w, h);
  }
}
class ImageData {
  constructor(a, b, c) {
    if (typeof a === 'number') {
      this.width = a; this.height = b;
      this.data = new Uint8ClampedArray(a * b * 4);
    } else {
      this.data = a instanceof Uint8ClampedArray ? a : new Uint8ClampedArray(a);
      this.width = b; this.height = c === undefined ? this.data.length / 4 / b : c;
    }
  }
}
global.CanvasRenderingContext2D = CanvasRenderingContext2D;
global.CanvasGradient = CanvasGradient;
global.Path2D = Path2D;
global.ImageData = ImageData;

class HTMLCanvasElement extends HTMLElement {
  getContext(kind) {
    kind = String(kind === undefined ? '' : kind);
    if (kind === '2d') {
      if (!this._ctx2d) this._ctx2d = new CanvasRenderingContext2D(this);
      else N('canvas_ensure', this[ID]);
      return this._ctx2d;
    }
    // Minimal WebGL subset over the shared scene store (see the 3D section above).
    if (kind === 'webgl' || kind === 'experimental-webgl') {
      if (!this._ctxGL) this._ctxGL = new WebGLRenderingContext(this);
      else N('canvas_ensure', this[ID]);
      return this._ctxGL;
    }
    if (kind === 'webgl2') {
      if (!this._ctxGL2) this._ctxGL2 = new WebGL2RenderingContext(this);
      else N('canvas_ensure', this[ID]);
      return this._ctxGL2;
    }
    // 'webgpu'/'bitmaprenderer': not implemented — null for feature detection.
    return null;
  }
  toDataURL(type) {
    if (type !== undefined && String(type) !== 'image/png') { /* only image/png; still returns PNG */ }
    if (this.width === 0 || this.height === 0) return 'data:,';
    return N('canvas_data_url', this[ID]);
  }
  toBlob(callback, type) {
    if (typeof callback !== 'function') throw new TypeError('Failed to execute \'toBlob\' on \'HTMLCanvasElement\': parameter 1 is not of type \'Function\'.');
    if (this.width === 0 || this.height === 0) { callback(null); return; }
    const url = N('canvas_data_url', this[ID]);
    if (url === 'data:,') { callback(null); return; }
    const bytes = b64ToBytes(url.slice(url.indexOf(',') + 1));
    // PNG is the only encoding the raster backend exposes; other requested types fall back to it.
    callback(new Blob([bytes], { type: 'image/png' }));
  }
  get width() { return parseInt(this.getAttribute('width') || '300', 10); } set width(v) { this.setAttribute('width', String(v)); N('canvas_resize', this[ID]); if (this._ctx2d) this._ctx2d._resetCache(); }
  get height() { return parseInt(this.getAttribute('height') || '150', 10); } set height(v) { this.setAttribute('height', String(v)); N('canvas_resize', this[ID]); if (this._ctx2d) this._ctx2d._resetCache(); }
}
// OffscreenCanvas is backed by a detached native canvas sharing the Canvas 2D rasterizer.
// It never enters the display list, so it can be drawn into and read back but not painted
// directly; `transferToImageBitmap()` yields a source usable by `drawImage`.
class ImageBitmap {
  constructor(canvas) { this[ID] = canvas[ID]; this.width = canvas.width; this.height = canvas.height; this._canvas = canvas; }
  close() { /* the backing canvas is owned by the OffscreenCanvas that produced it */ }
}
class OffscreenCanvas {
  constructor(w = 300, h = 150) {
    const el = document.createElement('canvas');
    el.width = w; el.height = h;
    this._el = el;
    this._ctx = null;
  }
  get width() { return this._el.width; }
  set width(v) { this._el.width = v; if (this._ctx) this._ctx._resetCache(); }
  get height() { return this._el.height; }
  set height(v) { this._el.height = v; if (this._ctx) this._ctx._resetCache(); }
  getContext(kind) {
    if (kind === '2d') {
      if (!this._ctx) this._ctx = this._el.getContext('2d');
      return this._ctx;
    }
    return null;
  }
  convertToBlob(opts) {
    const type = opts && opts.type;
    return new Promise(resolve => this._el.toBlob(resolve, type));
  }
  transferToImageBitmap() { return new ImageBitmap(this._el); }
}
class HTMLStyleElement extends HTMLElement { get sheet() { return null; } }
class HTMLLinkElement extends HTMLElement {}
reflectStr(HTMLLinkElement.prototype, 'href'); reflectStr(HTMLLinkElement.prototype, 'rel');
class HTMLScriptElement extends HTMLElement {}
reflectStr(HTMLScriptElement.prototype, 'src'); reflectStr(HTMLScriptElement.prototype, 'type');

// ----- 3D scenes (v1 subset) ------------------------------------------------------------------
// Primary API: `RendrixScene` (desc-based handles on `<canvas>`; the desc schema is shared
// with the future GPU scene backend, so scripts never change). Aliases over the same
// native store (`scene3d_upsert`/`scene3d_remove`/`scene3d_get`):
//   - `Rendrix3D.createBoxScene({...})` builds a one-box desc,
//   - `canvas.getContext('webgl'|'webgl2')` returns a minimal WebGL-subset context
//     (clearColor/clear/enable/viewport, buffer upload for triangle soup, drawArrays).
// `<rendrix-scene>` is a declarative host: scenes attach, round-trip and remove exactly
// like on canvas, but compositing requires `<canvas>` in v1 (paint.rs only composites
// `canvas` tags), so `rendrix-scene` hosts are API-valid and forward-compatible, not
// yet visible. Unsupported GL calls warn and no-op (or return null) — never crash.

const SCENE_HOST_TAGS = ['canvas', 'rendrix-scene'];
function resolveSceneHost(t) {
  const el = typeof t === 'string' ? document.getElementById(t) : t;
  if (!el || !(el instanceof Element)) throw new TypeError('RendrixScene: canvas or element id expected');
  if (!SCENE_HOST_TAGS.includes(el.localName)) throw new TypeError('RendrixScene requires a <canvas> or <rendrix-scene> element');
  return el;
}
function num3(v, d) {
  return Array.isArray(v) && v.length >= 3 && v.slice(0, 3).every(x => Number.isFinite(+x))
    ? [+v[0], +v[1], +v[2]] : d.slice();
}
function scale3(v) {
  if (Number.isFinite(+v) && +v !== 0) return [+v, +v, +v];
  if (Array.isArray(v) && v.length >= 3 && v.slice(0, 3).every(x => Number.isFinite(+x))) return [+v[0], +v[1], +v[2]];
  return [1, 1, 1];
}
function normalizeMesh(m, i) {
  if (!m || typeof m !== 'object') throw new TypeError(`RendrixScene: mesh ${i} must be an object`);
  const geo = m.geo && typeof m.geo === 'object' ? { ...m.geo } : {};
  if (typeof geo.kind !== 'string' || !geo.kind) throw new DOMException(`RendrixScene: mesh ${i} needs geo.kind`, 'NotSupportedError');
  const mat = m.material && typeof m.material === 'object' ? m.material : {};
  const xf = m.transform && typeof m.transform === 'object' ? m.transform : {};
  return {
    geo,
    material: {
      color: mat.color === undefined ? [1, 1, 1, 1] : mat.color,
      metal: Number.isFinite(+mat.metal) ? +mat.metal : 0,
      rough: Number.isFinite(+mat.rough) ? +mat.rough : 0.8,
      emissive: Number.isFinite(+mat.emissive) ? Math.min(1, Math.max(0, +mat.emissive)) : 0,
    },
    transform: {
      pos: num3(xf.pos, [0, 0, 0]),
      rotEulerDeg: num3(xf.rotEulerDeg !== undefined ? xf.rotEulerDeg : xf.rot, [0, 0, 0]),
      scale: scale3(xf.scale),
    },
  };
}
function defaultSceneCamera() { return { fovY: 45, near: 0.1, far: 100, position: [0, 0, 5], lookAt: [0, 0, 0] }; }
function normalizeDesc(desc) {
  if (!desc || typeof desc !== 'object') throw new TypeError('RendrixScene: scene description must be an object');
  const cam = desc.camera && typeof desc.camera === 'object' ? desc.camera : {};
  const d = defaultSceneCamera();
  return {
    camera: {
      fovY: Number.isFinite(+cam.fovY) ? +cam.fovY : d.fovY,
      near: Number.isFinite(+cam.near) && +cam.near > 0 ? +cam.near : d.near,
      far: Number.isFinite(+cam.far) ? +cam.far : d.far,
      position: num3(cam.position, d.position),
      lookAt: num3(cam.lookAt, d.lookAt),
    },
    meshes: Array.isArray(desc.meshes) ? desc.meshes.map(normalizeMesh) : [],
    lights: Array.isArray(desc.lights) && desc.lights.length ? desc.lights : [{ dir: [0.4, 0.8, 0.6] }],
    background: desc.background !== undefined ? desc.background : (desc.clearColor !== undefined ? desc.clearColor : [0, 0, 0, 0]),
  };
}
class RendrixSceneHandle {
  constructor(el, desc) { Object.defineProperty(this, '_el', { value: el }); this._desc = desc; }
  get canvas() { return this._el; }
  get desc() { return JSON.parse(JSON.stringify(this._desc)); }
  update(desc) { this._desc = normalizeDesc(desc); mut('scene3d_upsert', this._el[ID], JSON.stringify(this._desc)); return this; }
  setCamera(cam) {
    const full = normalizeDesc({ camera: { ...this._desc.camera, ...(cam || {}) }, meshes: [], lights: this._desc.lights });
    this._desc.camera = full.camera;
    mut('scene3d_upsert', this._el[ID], JSON.stringify(this._desc));
    return this;
  }
  _add(geo, opts = {}) {
    const mesh = normalizeMesh({
      geo,
      material: { color: opts.color !== undefined ? opts.color : [1, 1, 1, 1], emissive: opts.emissive || 0 },
      transform: { pos: opts.pos || opts.position || [0, 0, 0], rotEulerDeg: opts.rot || opts.rotEulerDeg || [0, 0, 0], scale: opts.scale === undefined ? 1 : opts.scale },
    }, this._desc.meshes.length);
    this._desc.meshes.push(mesh);
    mut('scene3d_upsert', this._el[ID], JSON.stringify(this._desc));
    return this._desc.meshes.length - 1;
  }
  addBox(opts) { return this._add({ kind: 'box', size: (opts && opts.size !== undefined) ? opts.size : 1 }, opts); }
  addPlane(opts) { return this._add({ kind: 'plane', size: (opts && opts.size !== undefined) ? opts.size : 1 }, opts); }
  remove() { N('scene3d_remove', this._el[ID]); }
}
const RendrixScene = {
  createScene(target, desc = {}) {
    const el = resolveSceneHost(target);
    const full = normalizeDesc(desc);
    mut('scene3d_upsert', el[ID], JSON.stringify(full));
    return new RendrixSceneHandle(el, full);
  },
  getScene(target) {
    const el = typeof target === 'string' ? document.getElementById(target) : target;
    if (!el || !(el instanceof Element) || !SCENE_HOST_TAGS.includes(el.localName)) return null;
    const raw = N('scene3d_get', el[ID]);
    return raw === null ? null : JSON.parse(raw);
  },
};
const Rendrix3D = {
  // Convenience desc builder over the primary RendrixScene API.
  createBoxScene({ color = [0.3, 0.6, 1, 1], fovY = 40, camPos = [0, 0, 6], size = 2, angle = [25, 30, 0] } = {}) {
    return {
      camera: { fovY, near: 0.1, far: 100, position: camPos.slice(), lookAt: [0, 0, 0] },
      meshes: [{ geo: { kind: 'box', size }, material: { color: color.slice ? color.slice() : color }, transform: { pos: [0, 0, 0], rotEulerDeg: angle.slice(), scale: 1 } }],
      lights: [{ dir: [0.4, 0.8, 0.6] }],
      background: [0, 0, 0, 0],
    };
  },
};
class WebGLLikeContext {
  constructor(el) {
    Object.defineProperty(this, '_cid', { value: el[ID] });
    this._clear = [0, 0, 0, 0];
    this._color = [1, 1, 1, 1];
    this._frames = [];
    this._camera = defaultSceneCamera();
    this._buffer = null;
    this._depthTest = false;
    this._viewport = null;
    N('canvas_ensure', this._cid);
    // Empty + transparent: the flush skips rendering, so a bare getContext probe can
    // never wipe 2D content — the scene entry only engages the lifecycle.
    mut('scene3d_upsert', this._cid, JSON.stringify({ camera: this._camera, meshes: [], lights: [{ dir: [0.4, 0.8, 0.6] }], background: this._clear }));
  }
  get canvas() { return wrap(this._cid); }
  get drawingBufferWidth() { const c = this.canvas; return c ? c.width : 0; }
  get drawingBufferHeight() { const c = this.canvas; return c ? c.height : 0; }
  _push() { mut('scene3d_upsert', this._cid, JSON.stringify({ camera: this._camera, meshes: this._frames, lights: [{ dir: [0.4, 0.8, 0.6] }], background: this._clear })); }
  clearColor(r, g, b, a) { this._clear = [+r || 0, +g || 0, +b || 0, a === undefined ? 1 : +a]; }
  clear(mask) {
    mask = +mask || 0;
    const known = WebGLLikeContext.COLOR_BUFFER_BIT | WebGLLikeContext.DEPTH_BUFFER_BIT;
    if (mask & ~known) console.warn(`[webgl] clear: unsupported mask bits 0x${(mask & ~known).toString(16)} ignored`);
    if (mask & WebGLLikeContext.COLOR_BUFFER_BIT) { this._frames = []; this._push(); }
  }
  enable(cap) {
    if (cap === WebGLLikeContext.DEPTH_TEST) this._depthTest = true;
    else console.warn(`[webgl] enable: capability ${cap} is not supported (ignored)`);
  }
  disable(cap) {
    if (cap === WebGLLikeContext.DEPTH_TEST) this._depthTest = false;
    else console.warn(`[webgl] disable: capability ${cap} is not supported (ignored)`);
  }
  viewport(x, y, w, h) { this._viewport = [+x || 0, +y || 0, +w || 0, +h || 0]; }
  createBuffer() { return { _buf: null }; }
  bindBuffer(target, buf) {
    if (target !== WebGLLikeContext.ARRAY_BUFFER) { console.warn(`[webgl] bindBuffer: target ${target} is not supported (ignored)`); return; }
    this._bound = buf || null;
  }
  bufferData(target, data, usage) {
    if (target !== WebGLLikeContext.ARRAY_BUFFER) { console.warn(`[webgl] bufferData: target ${target} is not supported (ignored)`); return; }
    let arr = null;
    if (ArrayBuffer.isView(data)) arr = Float32Array.from(data, Number);
    else if (data instanceof ArrayBuffer) arr = new Float32Array(data);
    else if (Array.isArray(data)) arr = new Float32Array(data.map(Number));
    else { console.warn('[webgl] bufferData: data must be an ArrayBufferView, ArrayBuffer or array (ignored)'); return; }
    this._buffer = arr;
  }
  createShader() { return { _shader: true }; }
  shaderSource() {} compileShader() {}
  getShaderParameter() { return true; }
  createProgram() { return { _prog: true }; }
  attachShader() {} linkProgram() {} useProgram() {}
  getProgramParameter() { return true; }
  getAttribLocation() { return { _attrib: 0 }; }
  getUniformLocation() { return { _uniform: 0 }; }
  vertexAttribPointer() {} enableVertexAttribArray() {} disableVertexAttribArray() {}
  vertexAttrib1f() {} vertexAttrib2f() {} vertexAttrib3f() {} vertexAttrib4f() {}
  uniform1f() {} uniform1i() {} uniform2f() {} uniform3f() {}
  uniform4f(loc, r, g, b, a) { this._color = [+r || 0, +g || 0, +b || 0, a === undefined ? 1 : +a]; }
  uniform4fv(loc, v) { const a = Array.from(v || []); this.uniform4f(loc, a[0], a[1], a[2], a[3]); }
  drawArrays(mode, first, count) {
    if (mode !== WebGLLikeContext.TRIANGLES) { console.warn(`[webgl] drawArrays: mode ${mode} is not supported (only TRIANGLES)`); return; }
    first = Math.max(0, Math.floor(+first) || 0); count = Math.max(0, Math.floor(+count) || 0);
    const src = this._buffer;
    if (!src || (first + count) * 3 > src.length || count % 3 !== 0) { console.warn('[webgl] drawArrays: no buffer data for the requested range (call bufferData first)'); return; }
    this._frames.push({
      geo: { kind: 'tris', verts: Array.from(src.slice(first * 3, (first + count) * 3)) },
      material: { color: this._color.slice() },
      transform: { pos: [0, 0, 0], rotEulerDeg: [0, 0, 0], scale: 1 },
    });
    this._push();
  }
  drawElements() { console.warn('[webgl] drawElements is not supported in Rendrix (use drawArrays)'); }
  getParameter(p) {
    const C = WebGLLikeContext;
    if (p === C.VERSION) return 'WebGL 1.0 (Rendrix software rasterizer)';
    if (p === C.RENDERER) return 'Rendrix';
    if (p === C.VENDOR) return 'Rendrix';
    if (p === C.COLOR_CLEAR_VALUE) return this._clear.slice();
    if (p === C.VIEWPORT) return this._viewport || [0, 0, this.drawingBufferWidth, this.drawingBufferHeight];
    if (p === C.DEPTH_TEST) return this._depthTest;
    console.warn(`[webgl] getParameter: parameter ${p} is not supported (null)`);
    return null;
  }
  getError() { return WebGLLikeContext.NO_ERROR; }
  isContextLost() { return false; }
  getContextAttributes() { return { alpha: true, depth: false, stencil: false, antialias: false, premultipliedAlpha: true, preserveDrawingBuffer: false }; }
  getExtension() { return null; }
  getSupportedExtensions() { return []; }
  flush() {} finish() {}
}
WebGLLikeContext.DEPTH_TEST = 0x0B71; WebGLLikeContext.COLOR_BUFFER_BIT = 0x4000; WebGLLikeContext.DEPTH_BUFFER_BIT = 0x0100;
WebGLLikeContext.TRIANGLES = 0x0004; WebGLLikeContext.ARRAY_BUFFER = 0x8892; WebGLLikeContext.ELEMENT_ARRAY_BUFFER = 0x8893;
WebGLLikeContext.STATIC_DRAW = 0x88E4; WebGLLikeContext.DYNAMIC_DRAW = 0x88E8; WebGLLikeContext.STREAM_DRAW = 0x88E0;
WebGLLikeContext.FLOAT = 0x1406; WebGLLikeContext.VERTEX_SHADER = 0x8B31; WebGLLikeContext.FRAGMENT_SHADER = 0x8B30;
WebGLLikeContext.COMPILE_STATUS = 0x8B81; WebGLLikeContext.LINK_STATUS = 0x8B82;
WebGLLikeContext.NO_ERROR = 0; WebGLLikeContext.VERSION = 0x1F02; WebGLLikeContext.VENDOR = 0x1F00; WebGLLikeContext.RENDERER = 0x1F01;
WebGLLikeContext.VIEWPORT = 0x0BA2; WebGLLikeContext.COLOR_CLEAR_VALUE = 0x0C22;
for (const name of ['createTexture', 'bindTexture', 'texImage2D', 'texSubImage2D', 'texParameteri', 'generateMipmap',
    'createFramebuffer', 'bindFramebuffer', 'framebufferTexture2D', 'checkFramebufferStatus',
    'createRenderbuffer', 'bindRenderbuffer', 'renderbufferStorage',
    'blendFunc', 'depthFunc', 'depthMask', 'clearDepth', 'cullFace', 'frontFace', 'scissor', 'polygonOffset']) {
  WebGLLikeContext.prototype[name] = function () { console.warn(`[webgl] ${name} is not supported in Rendrix (ignored)`); return null; };
}
// Browser-faithful access: constants work on the context object too (`gl.TRIANGLES`).
for (const k of Object.getOwnPropertyNames(WebGLLikeContext)) {
  if (typeof WebGLLikeContext[k] === 'number') WebGLLikeContext.prototype[k] = WebGLLikeContext[k];
}
class WebGL2RenderingContext extends WebGLLikeContext {}
// Alias kept for feature-detection code that names the 1.0 type.
class WebGLRenderingContext extends WebGLLikeContext {}
class RendrixSceneElement extends HTMLElement {
  getContext(kind) {
    kind = String(kind === undefined ? '' : kind);
    if (kind === 'webgl' || kind === 'experimental-webgl' || kind === 'webgl2') {
      if (!this._ctx3d) this._ctx3d = new WebGLLikeContext(this);
      return this._ctx3d;
    }
    return null;
  }
  get width() { return parseInt(this.getAttribute('width') || '300', 10); } set width(v) { this.setAttribute('width', String(v)); }
  get height() { return parseInt(this.getAttribute('height') || '150', 10); } set height(v) { this.setAttribute('height', String(v)); }
}

const SPECIAL = { input: HTMLInputElement, textarea: HTMLTextAreaElement, select: HTMLSelectElement, option: HTMLOptionElement, button: HTMLButtonElement, form: HTMLFormElement,
  a: HTMLAnchorElement, label: HTMLLabelElement, img: HTMLImageElement, canvas: HTMLCanvasElement, 'rendrix-scene': RendrixSceneElement, style: HTMLStyleElement, link: HTMLLinkElement, script: HTMLScriptElement };
function elementClass(tag) { return SPECIAL[tag] || tagClasses.get(tag) || HTMLElement; }

// ----- Document -------------------------------------------------------------------------------

let readyState = 'loading';
class Document extends Node {
  get documentElement() { return wrap(N('document_element')); }
  get head() { return wrap(N('head')); }
  get body() { return wrap(N('body')); }
  set body(v) { const old = this.body; if (old) old.replaceWith(v); }
  get title() { const t = this.querySelector('title'); return t ? t.textContent.trim() : ''; }
  set title(v) { let t = this.querySelector('title'); if (!t) { t = this.createElement('title'); this.head.appendChild(t); } t.textContent = String(v); N('window_title', String(v)); }
  get readyState() { return readyState; }
  get URL() { return location.href; }
  get documentURI() { return location.href; }
  get location() { return location; }
  get defaultView() { return global; }
  get characterSet() { return 'UTF-8'; }
  get contentType() { return 'text/html'; }
  get compatMode() { return 'CSS1Compat'; }
  get visibilityState() { return 'visible'; }
  get hidden() { return false; }
  get activeElement() { const f = N('focused'); return f === null ? this.body : wrap(f); }
  get fonts() { return { ready: Promise.resolve(), load: () => Promise.resolve([]), check: () => true, add() {}, delete() {} }; }
  get styleSheets() { return []; }
  get adoptedStyleSheets() { return []; }
  set adoptedStyleSheets(v) { for (const s of v || []) if (s && s._cssText) N('add_stylesheet', s._cssText); }
  get cookie() { return N('cookie_get'); }
  set cookie(v) { N('cookie_set', String(v)); }
  hasFocus() { return true; }
  get designMode() { return designModeValue; }
  set designMode(v) { designModeValue = String(v).toLowerCase() === 'on' ? 'on' : 'off'; }
  createElement(tag) {
    const el = wrap(N('create_element', String(tag)));
    try { upgradeCustom(el); } catch (_) {}
    return el;
  }
  createElementNS(ns, tag) { const el = wrap(N('create_element', String(tag))); try { upgradeCustom(el); } catch (_) {} return el; }
  createTextNode(t) { return wrap(N('create_text', String(t))); }
  createComment(t) { return wrap(N('create_comment', String(t))); }
  createDocumentFragment() { return wrap(N('create_fragment')); }
  createEvent(type) { return /mouse/i.test(type) ? new MouseEvent('') : /key/i.test(type) ? new KeyboardEvent('') : new Event(''); }
  createRange() { return new Range(); }
  importNode(n, deep) { return n.cloneNode(deep); }
  adoptNode(n) { return n; }
  getSelection() { return docSelection; }
  elementFromPoint(x, y) { return wrap(N('hit_test', +x, +y)); }
  elementsFromPoint(x, y) { const out = []; for (let e = this.elementFromPoint(x, y); e; e = e.parentElement) out.push(e); return out; }
  getElementsByTagName(t) { return HTMLCollection.from(wrapAll(N('by_tag', this[ID], String(t)))); }
  getElementsByClassName(c) { return HTMLCollection.from(wrapAll(N('by_class', this[ID], String(c)))); }
  execCommand(cmd, _ui, value) {
    cmd = String(cmd || '').toLowerCase();
    if (cmd === 'inserttext' && value !== undefined) {
      const ae = this.activeElement;
      try {
        if (ae && ae.isContentEditable) {
          // Append into the first text child or textContent (minimal editing).
          const fc = ae.firstChild;
          if (fc && fc.nodeType === 3) fc.data = (fc.data || '') + String(value);
          else ae.textContent = (ae.textContent || '') + String(value);
          return true;
        }
      } catch (_) {}
      return false;
    }
    if (cmd === 'delete' || cmd === 'forwarddelete') {
      try { docSelection.deleteFromDocument(); return true; } catch (_) { return false; }
    }
    if (cmd === 'selectall') {
      try { docSelection.selectAllChildren(this.activeElement || this.body); return true; } catch (_) { return false; }
    }
    return false;
  }
  _eventParent() { return windowTarget; }
}
parentNodeMixin(Document.prototype);

const document = wrap(N('document_root'));
