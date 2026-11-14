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
  get isContentEditable() { return false; }
  setPointerCapture() {} releasePointerCapture() {} hasPointerCapture() { return false; }
  attachShadow() { throw new DOMException('Shadow DOM is not supported', 'NotSupportedError'); }
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
  get selectionStart() { const s = N('selection', this[ID]); return s ? s[0] : null; }
  set selectionStart(v) { const s = N('selection', this[ID]); N('set_selection', this[ID], +v, s[1]); }
  get selectionEnd() { const s = N('selection', this[ID]); return s ? s[1] : null; }
  set selectionEnd(v) { const s = N('selection', this[ID]); N('set_selection', this[ID], s[0], +v); }
  get selectionDirection() { return 'none'; }
  setSelectionRange(a, b) { N('set_selection', this[ID], a, b); }
  select() { N('set_selection', this[ID], 0, this.value.length); }
  setRangeText(t, s, e) { const v = this.value; s = s ?? this.selectionStart; e = e ?? this.selectionEnd; this.value = v.slice(0, s) + t + v.slice(e); }
  get form() { return formOf(this); }
  get files() { return null; }
  get labels() { return NodeList.from([]); }
  get validity() { return { valid: true, valueMissing: false, typeMismatch: false, patternMismatch: false, tooLong: false, tooShort: false, rangeUnderflow: false, rangeOverflow: false, stepMismatch: false, badInput: false, customError: false }; }
  get willValidate() { return !this.disabled; }
  checkValidity() { return true; } reportValidity() { return true; } setCustomValidity() {}
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
  checkValidity() { return true; } reportValidity() { return true; } setCustomValidity() {}
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
  checkValidity() { return true; } reportValidity() { return true; }
}
for (const p of ['name']) reflectStr(HTMLSelectElement.prototype, p);
for (const p of ['disabled', 'required', 'multiple']) reflectBool(HTMLSelectElement.prototype, p);

class HTMLButtonElement extends HTMLElement {
  get type() { const t = (this.getAttribute('type') || 'submit').toLowerCase(); return ['button', 'reset', 'submit'].includes(t) ? t : 'submit'; }
  set type(v) { this.setAttribute('type', v); }
  get form() { return formOf(this); }
  get value() { return this.getAttribute('value') || ''; }
  set value(v) { this.setAttribute('value', v); }
  checkValidity() { return true; } reportValidity() { return true; }
}
reflectStr(HTMLButtonElement.prototype, 'name'); reflectBool(HTMLButtonElement.prototype, 'disabled'); reflectBool(HTMLButtonElement.prototype, 'autofocus');

class HTMLFormElement extends HTMLElement {
  get elements() { return HTMLCollection.from(this.querySelectorAll('input, select, textarea, button')); }
  get length() { return this.elements.length; }
  submit() { /* no navigation in Rendrix */ }
  requestSubmit(submitter) {
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
  checkValidity() { return true; } reportValidity() { return true; }
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
  constructor(x0, y0, x1, y1) { this._x0 = +x0; this._y0 = +y0; this._x1 = +x1; this._y1 = +y1; this._stops = []; }
  addColorStop(offset, color) {
    offset = +offset;
    if (!Number.isFinite(offset) || offset < 0 || offset > 1) throw new DOMException('The provided float value is non-finite', 'SyntaxError');
    this._stops.push([offset, String(color)]);
  }
}
class CanvasRenderingContext2D {
  constructor(canvas) {
    Object.defineProperty(this, '_id', { value: canvas[ID] });
    this._resetCache();
    N('canvas_ensure', this._id);
  }
  _resetCache() { this._fill = '#000000'; this._stroke = '#000000'; }
  get canvas() { return wrap(this._id); }
  // ----- state -----
  save() { N('canvas_save', this._id); }
  restore() { N('canvas_restore', this._id); }
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
      return JSON.stringify({ grad: { x0: v._x0, y0: v._y0, x1: v._x1, y1: v._y1, stops: v._stops } });
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
  // ----- paths -----
  beginPath() { N('canvas_begin', this._id); }
  moveTo(x, y) { N('canvas_move_to', this._id, +x, +y); }
  lineTo(x, y) { N('canvas_line_to', this._id, +x, +y); }
  rect(x, y, w, h) { N('canvas_rect', this._id, +x, +y, +w, +h); }
  arc(x, y, r, a0, a1, ccw) { mut('canvas_arc', this._id, +x, +y, +r, +a0, +a1, !!ccw); }
  ellipse(x, y, rx, ry, rot, a0, a1, ccw) { mut('canvas_ellipse', this._id, +x, +y, +rx, +ry, +rot, +a0, +a1, !!ccw); }
  closePath() { N('canvas_close', this._id); }
  fill() { N('canvas_fill', this._id); }
  stroke() { N('canvas_stroke', this._id); }
  fillRect(x, y, w, h) { N('canvas_fill_rect', this._id, +x, +y, +w, +h); }
  strokeRect(x, y, w, h) { N('canvas_stroke_rect', this._id, +x, +y, +w, +h); }
  clearRect(x, y, w, h) { N('canvas_clear_rect', this._id, +x, +y, +w, +h); }
  createLinearGradient(x0, y0, x1, y1) { return new CanvasGradient(x0, y0, x1, y1); }
  createRadialGradient() { throw new DOMException('Radial gradients are not supported', 'NotSupportedError'); }
  createPattern() { throw new DOMException('Patterns are not supported', 'NotSupportedError'); }
  // ----- images -----
  drawImage(src, ...a) {
    let sj, sx, sy, sw, sh, dx, dy, dw, dh;
    if (src instanceof HTMLCanvasElement) {
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
  get width() { return parseInt(this.getAttribute('width') || '300', 10); } set width(v) { this.setAttribute('width', String(v)); N('canvas_resize', this[ID]); if (this._ctx2d) this._ctx2d._resetCache(); }
  get height() { return parseInt(this.getAttribute('height') || '150', 10); } set height(v) { this.setAttribute('height', String(v)); N('canvas_resize', this[ID]); if (this._ctx2d) this._ctx2d._resetCache(); }
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
  get cookie() { return cookieJar; }
  set cookie(v) { cookieJar = String(v).split(';')[0]; }
  hasFocus() { return true; }
  createElement(tag) { return wrap(N('create_element', String(tag))); }
  createElementNS(ns, tag) { return wrap(N('create_element', String(tag))); }
  createTextNode(t) { return wrap(N('create_text', String(t))); }
  createComment(t) { return wrap(N('create_comment', String(t))); }
  createDocumentFragment() { return wrap(N('create_fragment')); }
  createEvent(type) { return /mouse/i.test(type) ? new MouseEvent('') : /key/i.test(type) ? new KeyboardEvent('') : new Event(''); }
  createRange() { throw new DOMException('Range is not supported', 'NotSupportedError'); }
  importNode(n, deep) { return n.cloneNode(deep); }
  adoptNode(n) { return n; }
  getSelection() { return { rangeCount: 0, toString: () => '', removeAllRanges() {}, addRange() {} }; }
  elementFromPoint(x, y) { return wrap(N('hit_test', +x, +y)); }
  elementsFromPoint(x, y) { const out = []; for (let e = this.elementFromPoint(x, y); e; e = e.parentElement) out.push(e); return out; }
  getElementsByTagName(t) { return HTMLCollection.from(wrapAll(N('by_tag', this[ID], String(t)))); }
  getElementsByClassName(c) { return HTMLCollection.from(wrapAll(N('by_class', this[ID], String(c)))); }
  execCommand() { return false; }
  _eventParent() { return windowTarget; }
}
parentNodeMixin(Document.prototype);
let cookieJar = '';

const document = wrap(N('document_root'));
