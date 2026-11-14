// Minimal ambient types for the adapter surface. The real checking power comes from the
// `solid-js` types in the consuming app's own install (`npm i -D solid-js` or the dependency
// declared here); this file only keeps `tsc --noEmit` quiet for the re-export names.
export * from 'solid-js';
export { render } from 'solid-js/web';
export { default as h } from 'solid-js/h';
import hDefault from 'solid-js/h';
export declare const Fragment: typeof hDefault.Fragment;
