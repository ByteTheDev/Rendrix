// `rendrix-react-adapter/jsx-runtime`: a `createElement`-compatible `jsx` pragma runtime.
//
// `buildApp` pins the TypeScript `jsxImportSource` to `rendrix` (whose runtime is baked into
// the engine), so `.jsx`/`.tsx` files always compile to the direct-DOM `rendrix/jsx-runtime`
// and can never address this module today. This file exists for two supported uses:
//   1. hand-written `jsx(type, props)` / `jsxs` calls (same shape the TS transform would emit),
//   2. non-Rendrix bundlers, where `jsxImportSource: 'rendrix-react-adapter'` resolves here.
//
// Semantics mirror `createElement(type, config, ...children)`: props are plain values
// re-read on every render (there are no Solid-style accessors in React). Components are
// plain functions. See `../README.md` for the props model this mapping relies on.
//
// The shim must run before any `react` module body: keep this import first.
import './shim.js';
import { createElement, Fragment } from './index.js';

function toChildren(children) {
  if (children === undefined) return [];
  return Array.isArray(children) ? children : [children];
}

export function jsx(type, props, key) {
  props = props || {};
  const { children, ...rest } = props;
  const config = key !== undefined ? { ...rest, key } : rest;
  return createElement(type, config, ...toChildren(children));
}

export const jsxs = jsx;

export function jsxDEV(type, props, key) {
  return jsx(type, props, key);
}

export { Fragment };
