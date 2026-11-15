// Mutation-mode host config: React reconciler operations mapped onto the Rendrix DOM.
//
// Coverage: `document.createElement` / `createTextNode`, attribute/property patching,
// `on*` events via `addEventListener` (per element, no delegation), `style` objects and
// strings, `className`, `dangerouslySetInnerHTML`, text updates, and Offscreen hide/unhide
// for `Suspense` fallbacks. See `../README.md` for the deliberate divergences
// (`onChange` normalization, controlled inputs, no SVG namespace).

import { DefaultEventPriority, DiscreteEventPriority, NoEventPriority } from './vendor/constants.js';

// React schedules event-handler updates on the sync lane and everything else
// concurrently. The reconciler delegates priority to the host: `setCurrentUpdatePriority`
// / `getCurrentUpdatePriority` bracket batched scopes (events, `flushSync`), and
// `resolveUpdatePriority` maps the ambient priority to an update lane — exactly like
// react-dom does. Event listeners attached below run wrapped at discrete priority;
// anything else resolves to the default lane. (The adapter's `render` mounts
// synchronously via `updateContainerSync` + `flushSync` — see `src/renderer.js`.)
let currentUpdatePriority = NoEventPriority;
let currentEventPriority = DefaultEventPriority;

export function getCurrentUpdatePriority() {
  return currentUpdatePriority;
}

export function setCurrentUpdatePriority(priority) {
  currentUpdatePriority = priority;
}

export function resolveUpdatePriority() {
  return currentUpdatePriority !== NoEventPriority ? currentUpdatePriority : currentEventPriority;
}

function withDiscretePriority(fn) {
  return function rendrixDiscreteEvent(...args) {
    const prev = currentEventPriority;
    currentEventPriority = DiscreteEventPriority;
    try {
      return fn.apply(this, args);
    } finally {
      currentEventPriority = prev;
    }
  };
}

// `onDoubleClick` is the only React event prop whose native name is not the
// lowercased suffix (`dblclick`).
const EVENT_ALIASES = { doubleclick: 'dblclick' };

// React prop names that do not match their DOM attribute names; anything else is
// passed through to `setAttribute` verbatim.
const ATTR_ALIASES = {
  tabIndex: 'tabindex', readOnly: 'readonly', maxLength: 'maxlength', minLength: 'minlength',
  colSpan: 'colspan', rowSpan: 'rowspan', useMap: 'usemap', frameBorder: 'frameborder',
  acceptCharset: 'accept-charset', accessKey: 'accesskey', autoComplete: 'autocomplete',
  autoFocus: 'autofocus', contentEditable: 'contenteditable', crossOrigin: 'crossorigin',
  dateTime: 'datetime', encType: 'enctype', formAction: 'formaction', formEncType: 'formenctype',
  formMethod: 'formmethod', formNoValidate: 'formnovalidate', formTarget: 'formtarget',
  hrefLang: 'hreflang', noValidate: 'novalidate', referrerPolicy: 'referrerpolicy',
};

const nativeEventName = prop => EVENT_ALIASES[prop.slice(2).toLowerCase()] || prop.slice(2).toLowerCase();

// React's `onChange` fires per keystroke on text fields. Rendrix dispatches native `input`
// for keystrokes and native `change` for checkboxes/radios/selects, so `onChange` is
// normalized to whichever native event the element actually fires (documented in README).
function changeNativeEvent(type, props) {
  if (type === 'textarea') return 'input';
  if (type === 'select') return 'change';
  if (type === 'input') {
    const t = String(props.type || 'text').toLowerCase();
    return t === 'checkbox' || t === 'radio' || t === 'file' ? 'change' : 'input';
  }
  return 'change';
}

function nativesFor(type, prop, props) {
  if (prop === 'onChange') return [changeNativeEvent(type, props)];
  // Focus/blur do not bubble natively; focusin/focusout do, which is what React's
  // onFocus/onBlur semantics require (the handler still receives a FocusEvent).
  if (prop === 'onFocus') return ['focusin'];
  if (prop === 'onBlur') return ['focusout'];
  return [nativeEventName(prop)];
}

function listenerMap(el) {
  return el.__rr_events || (el.__rr_events = new Map());
}

export function detachEvents(el, prop) {
  const m = el.__rr_events;
  if (!m) return;
  const e = m.get(prop);
  if (!e) return;
  for (const n of e.natives) el.removeEventListener(n, e.fn);
  m.delete(prop);
}

export function attachEvents(el, type, prop, props, fn) {
  detachEvents(el, prop);
  const natives = nativesFor(type, prop, props);
  const wrapped = withDiscretePriority(fn);
  listenerMap(el).set(prop, { natives, fn: wrapped });
  for (const n of natives) el.addEventListener(n, wrapped);
}

function applyStyle(el, value, prev) {
  if (typeof value === 'string') {
    if (value) el.setAttribute('style', value);
    else el.removeAttribute('style');
    return;
  }
  const next = value && typeof value === 'object' ? value : {};
  const old = prev && typeof prev === 'object' ? prev : {};
  for (const k of Object.keys(old)) {
    if (!(k in next)) {
      if (k.startsWith('--')) el.style.removeProperty(k);
      else el.style[k] = '';
    }
  }
  for (const k of Object.keys(next)) {
    const v = next[k];
    if (v == null || v === '') {
      if (k.startsWith('--')) el.style.removeProperty(k);
      else el.style[k] = '';
      continue;
    }
    if (k.startsWith('--')) el.style.setProperty(k, String(v));
    else el.style[k] = String(v);
  }
}

export function setProp(el, type, name, value, props) {
  if (name === 'children' || name === 'key' || name === 'ref') return;
  if (name.length > 2 && name.startsWith('on')) {
    // Only function handlers are wired; anything else is ignored (never set as an attribute).
    if (typeof value === 'function') attachEvents(el, type, name, props, value);
    else detachEvents(el, name);
    return;
  }
  if (name === 'className' || name === 'class') {
    if (value == null || value === false) el.removeAttribute('class');
    else el.setAttribute('class', Array.isArray(value) ? value.filter(Boolean).join(' ') : String(value));
    return;
  }
  if (name === 'style') {
    applyStyle(el, value, undefined);
    return;
  }
  if (name === 'dangerouslySetInnerHTML') {
    el.innerHTML = (value && value.__html) || '';
    return;
  }
  if (name === 'htmlFor') {
    if (value == null) el.removeAttribute('for');
    else el.setAttribute('for', String(value));
    return;
  }
  if (name === 'value') {
    const v = value == null ? '' : String(value);
    // Only form controls expose a `value` IDL property; anything else gets the attribute.
    if ('value' in el && el.value !== v) el.value = v;
    if (value == null) el.removeAttribute('value');
    else el.setAttribute('value', v);
    return;
  }
  if (name === 'checked' || name === 'selected') {
    if (name in el) el[name] = !!value;
    el.toggleAttribute(name, !!value);
    return;
  }
  if (name === 'defaultValue') {
    if (value == null) el.removeAttribute('value');
    else el.setAttribute('value', String(value));
    return;
  }
  if (name === 'defaultChecked') {
    el.toggleAttribute('checked', !!value);
    return;
  }
  if (value == null || value === false) {
    el.removeAttribute(ATTR_ALIASES[name] || name);
    return;
  }
  el.setAttribute(ATTR_ALIASES[name] || name, value === true ? '' : String(value));
}

export function removeProp(el, name, prev) {
  if (name === 'children' || name === 'key' || name === 'ref') return;
  if (name.length > 2 && name.startsWith('on')) {
    detachEvents(el, name);
    return;
  }
  if (name === 'className' || name === 'class') {
    el.removeAttribute('class');
    return;
  }
  if (name === 'style') {
    applyStyle(el, undefined, prev);
    return;
  }
  if (name === 'dangerouslySetInnerHTML') {
    el.innerHTML = '';
    return;
  }
  if (name === 'htmlFor') {
    el.removeAttribute('for');
    return;
  }
  if (name === 'value') {
    if ('value' in el) el.value = '';
    el.removeAttribute('value');
    return;
  }
  if (name === 'checked' || name === 'selected') {
    if (name in el) el[name] = false;
    el.removeAttribute(name);
    return;
  }
  el.removeAttribute(ATTR_ALIASES[name] || name);
}

export function shouldSetTextContent(type, props) {
  if (props.dangerouslySetInnerHTML != null) return false;
  const c = props.children;
  return typeof c === 'string' || typeof c === 'number';
}

export function createInstance(type, props) {
  const el = document.createElement(String(type));
  for (const k of Object.keys(props)) setProp(el, String(type), k, props[k], props);
  if (shouldSetTextContent(String(type), props)) {
    el.textContent = props.children == null ? '' : String(props.children);
  }
  return el;
}

export function commitUpdate(instance, type, oldProps, newProps) {
  if (shouldSetTextContent(type, newProps)) {
    const t = newProps.children == null ? '' : String(newProps.children);
    if (instance.textContent !== t) instance.textContent = t;
  } else if ((newProps.dangerouslySetInnerHTML && newProps.dangerouslySetInnerHTML.__html) !== (oldProps.dangerouslySetInnerHTML && oldProps.dangerouslySetInnerHTML.__html)) {
    instance.innerHTML = (newProps.dangerouslySetInnerHTML && newProps.dangerouslySetInnerHTML.__html) || '';
  }
  for (const k of Object.keys(oldProps)) {
    if (!(k in newProps)) removeProp(instance, k, oldProps[k]);
  }
  for (const k of Object.keys(newProps)) {
    if (oldProps[k] !== newProps[k]) {
      if (k === 'children' || k === 'key' || k === 'ref') continue;
      if (k === 'style') applyStyle(instance, newProps[k], oldProps[k]);
      else setProp(instance, type, k, newProps[k], newProps);
    }
  }
}

// Text hidden by Suspense/Offscreen keeps its value in a side table (a text node has no
// style to flip); unhide restores it verbatim.
const hiddenText = new WeakMap();

export const hostConfig = {
  supportsMutation: true,
  supportsPersistence: false,
  supportsHydration: false,
  isPrimaryRenderer: true,

  supportsMicrotasks: true,
  scheduleMicrotask:
    typeof queueMicrotask === 'function' ? queueMicrotask : fn => Promise.resolve().then(fn),
  scheduleTimeout:
    typeof setTimeout === 'function' ? (fn, ms) => setTimeout(fn, ms) : () => -1,
  cancelTimeout: typeof clearTimeout === 'function' ? id => clearTimeout(id) : () => {},
  noTimeout: -1,

  getCurrentUpdatePriority,
  setCurrentUpdatePriority,
  resolveUpdatePriority,

  getRootHostContext: () => null,
  getChildHostContext: parent => parent,

  shouldSetTextContent,
  createInstance,
  createTextInstance: text => document.createTextNode(String(text)),
  appendInitialChild: (parent, child) => parent.appendChild(child),
  finalizeInitialChildren: (el, type, props) => !!props.autoFocus,

  // React 19 removed `prepareUpdate` from the host contract: the host diffs old/new
  // props itself inside `commitUpdate` (signature below is positional).
  commitUpdate,
  commitTextUpdate: (textInstance, _oldText, newText) => {
    textInstance.nodeValue = String(newText);
  },
  // Switching a host child from text content to element children: drop the text so the
  // appended nodes do not concatenate onto it.
  resetTextContent: instance => {
    instance.textContent = '';
  },
  commitMount: (instance, _type, newProps) => {
    if (newProps.autoFocus && typeof instance.focus === 'function') {
      try {
        instance.focus();
      } catch { /* autofocus is best-effort */ }
    }
  },

  appendChild: (parent, child) => parent.appendChild(child),
  appendChildToContainer: (container, child) => container.appendChild(child),
  insertBefore: (parent, child, before) => parent.insertBefore(child, before),
  insertInContainerBefore: (container, child, before) => container.insertBefore(child, before),
  removeChild: (parent, child) => parent.removeChild(child),
  removeChildFromContainer: (container, child) => container.removeChild(child),

  hideInstance: el => el.style.setProperty('display', 'none', 'important'),
  unhideInstance: (el, props) => {
    const display = props.style && typeof props.style === 'object' ? props.style.display : undefined;
    if (display == null || display === '') el.style.removeProperty('display');
    else el.style.display = String(display);
  },
  hideTextInstance: textInstance => {
    hiddenText.set(textInstance, textInstance.nodeValue);
    textInstance.nodeValue = '';
  },
  unhideTextInstance: (textInstance, text) => {
    textInstance.nodeValue = hiddenText.has(textInstance) ? hiddenText.get(textInstance) : String(text);
    hiddenText.delete(textInstance);
  },

  getPublicInstance: instance => instance,
  getInstanceFromNode: () => null,
  getInstanceFromScope: () => null,
  prepareScopeUpdate: () => {},
  prepareForCommit: () => null,
  resetAfterCommit: () => {},
  preparePortalMount: container => container,
  detachDeletedInstance: () => {},
  maySuspendCommit: () => false,
  // The commit never waits: v1 has no suspensey resources (fonts/images block in
  // react-dom; here `waitForCommitToBeReady` reports ready immediately).
  startSuspendingCommit: () => ({}),
  waitForCommitToBeReady: () => null,
  // View transitions are out of scope: there is never an active transition to suspend on.
  suspendOnActiveViewTransition: () => {},
  getSuspendedCommitReason: () => null,
  clearContainer: container => {
    while (container.firstChild) container.removeChild(container.firstChild);
  },
  beforeActiveInstanceBlur: () => {
    // React calls this before hiding/removing a subtree that may hold focus.
    try {
      const active = document.activeElement;
      if (active && typeof active.blur === 'function') active.blur();
    } catch { /* best-effort */ }
  },
};
