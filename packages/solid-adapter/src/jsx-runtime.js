// `rendrix-solid-adapter/jsx-runtime`: an `h`-compatible `jsx` pragma runtime.
//
// `buildApp` pins the TypeScript `jsxImportSource` to `rendrix` (whose runtime is baked into
// the engine), so `.tsx` files always compile to the direct-DOM `rendrix/jsx-runtime` and can
// never address this module today. This file exists for two supported uses:
//   1. hand-written `jsx(type, props)` / `jsxs` calls (same shape the TS transform would emit),
//   2. non-Rendrix bundlers, where `jsxImportSource: 'rendrix-solid-adapter'` resolves here.
//
// Semantics mirror `h(type, props, ...children)`: props whose value is a function are
// dynamic (re-read on every reactive update); `children` may mix nodes, arrays and
// accessor functions. Components are plain functions of props. See `../README.md` for the
// `h` reactivity patterns this mapping relies on.
//
// The shim must run before any `solid-js` module body: keep this import first.
import './shim.js';
import h from 'solid-js/h';

function toChildren(children) {
  if (children === undefined) return [];
  return Array.isArray(children) ? children : [children];
}

export function jsx(type, props, key) {
  props = props || {};
  const { children, ...rest } = props;
  if (typeof type === 'function') {
    // Function components receive props (children included) like `h` would pass them.
    const full = { ...rest };
    if (children !== undefined) full.children = children;
    if (key !== undefined) full.key = key;
    return h(type, full);
  }
  return h(type, rest, ...toChildren(children));
}

export const jsxs = jsx;
export { Fragment } from './index.js';
