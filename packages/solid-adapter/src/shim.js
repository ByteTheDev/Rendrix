// Rendrix DOM compatibility shims for SolidJS (adapter-owned, not engine-owned).
//
// The Rendrix prelude intentionally exposes a small DOM subset, so a few globals Solid
// touches at runtime are missing. Each shim below is a no-op-compatible stub that lets the
// real `solid-js` client build run unmodified; the engine itself is untouched (this file ships
// with the adapter, so a future prelude addition simply overrides the `??=` assignment).
//
// Verified surface (solid-js@1.9.19, client `dist/solid.js` + `web/dist/web.js` + `h/dist/h.js`):
// - `SVGElement` is referenced by `h()` (`item instanceof SVGElement`) and by the web
//   runtime's `SVGElements` tag set. Rendrix has no SVG DOM, so the stub only satisfies the
//   `instanceof` checks (always false); `createElementNS('...svg...', tag)` falls back to a
//   plain element in the prelude, which is sufficient for smoke-rendering SVG tags as unknown
//   elements. Real SVG rendering needs engine support and is out of scope for this adapter.

globalThis.SVGElement ??= class SVGElement {};
