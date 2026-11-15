// `rendrix-react-adapter`: run React on Rendrix's DOM.
//
// The reconciler (`react-reconciler`, pinned in this package's `package.json`) drives
// updates; `src/host.js` maps its operations onto the real DOM — `document.createElement`,
// text/attribute patching, per-element `addEventListener` — so there is no virtual DOM and
// no `react-dom`. Mount with `render`, build trees with `h` (an alias for `createElement`);
// every other primitive (`useState`, `useEffect`, `useLayoutEffect`, `useMemo`, `Suspense`,
// `lazy`, …) is the real `react@19.3.1`, vendored as ESM in `src/vendor/` (see
// `scripts/vendor.mjs`: upstream ships CommonJS only, which the Rendrix ESM-only bundler
// cannot load).
//
// The shim MUST evaluate before any `react` module body runs: ES module evaluation is
// depth-first in source order, so this side-effect import stays on the first line.
import './shim.js';

export * from './vendor/react.js';
export { render, flushSync, batchedUpdates } from './renderer.js';

// `h` is `createElement` under the alias React-style `h` users expect (same role as
// Solid's `h`, but with React's re-render semantics: props are plain values, compared by
// identity on each render — see `../README.md`).
export { createElement as h } from './vendor/react.js';

import { useEffect as useEffectImpl } from './vendor/react.js';

/**
 * Run `fn` once after mount; its return value (if a function) runs on unmount.
 * The `onMount` equivalent: `useEffect(fn, [])` with cleanup support.
 */
export function onMount(fn) {
  useEffectImpl(() => {
    const cleanup = fn();
    return typeof cleanup === 'function' ? cleanup : undefined;
  }, []);
}

/**
 * Register `fn` to run on unmount (optionally re-registered per render).
 * The `onCleanup` equivalent: an effect whose only job is its cleanup.
 */
export function onCleanup(fn) {
  useEffectImpl(() => () => fn(), []);
}
