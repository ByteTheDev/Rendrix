// `rendrix-solid-adapter`: run SolidJS on Rendrix's DOM.
//
// Solid's fine-grained reactivity maps directly onto this DOM: signals patch text, attributes
// and classes in place — there is no virtual DOM and no second renderer. Mount with `render`,
// build trees with `h`, and keep every other primitive (`createSignal`, `createEffect`,
// `createMemo`, `onMount`, `onCleanup`, `Show`, `For`, `createComponent`, …) from `solid-js`.
//
// The shim MUST evaluate before any `solid-js` module body runs: ES module evaluation is
// depth-first in source order, so this side-effect import stays on the first line.
import './shim.js';

export * from 'solid-js';
// `render` (and only it) comes from the web runtime: it mounts a component into a container
// and returns a disposer that unmounts it. SSR/hydration entry points also exist in
// `solid-js/web` but have no Rendrix host, so they are deliberately not re-exported.
export { render } from 'solid-js/web';
export { default as h } from 'solid-js/h';
import hDefault from 'solid-js/h';
export const Fragment = hDefault.Fragment;
