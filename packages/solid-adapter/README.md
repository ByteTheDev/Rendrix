# rendrix-solid-adapter

Run [SolidJS](https://www.solidjs.com/) components on Rendrix's DOM. Solid's fine-grained
reactivity maps directly onto `document.createElement` / text / attribute patching — there is
**no virtual DOM and no second renderer**. The adapter is a thin, adapter-scoped package over
the real `solid-js@1.9.19` (declared in this package's own `package.json`; nothing else in the
monorepo depends on it).

```js
import { createSignal } from 'rendrix-solid-adapter';
import { render, h } from 'rendrix-solid-adapter';

function Counter() {
  const [n, setN] = createSignal(0);
  return h('button', { onClick: () => setN(n() + 1) }, () => `n=${n()}`);
}
const dispose = render(Counter, document.getElementById('root'));
// ... later: dispose() unmounts everything and runs all cleanups.
```

## Reactivity patterns with `h` (no compiler)

Without the Solid JSX compiler there is one rule to remember: **a prop is dynamic only if its
value is a function**. The compiler normally wraps expressions in accessors for you; with `h`
you do it by hand:

```js
// dynamic text / attributes: pass an accessor, not the value
h('button', { class: () => (on() ? 'on' : 'off') }, () => `n=${n()}`);
// static after first run (accessors nested inside are NOT tracked by h):
h('div', { classList: { on: () => on() } }); // AVOID — both classes stick
// dynamic class lists: lift the accessor to the top level instead:
h('div', { classList: () => ({ on: on() }) });
// dynamic children: accessors, arrays and components all compose:
h(Show, { when: show }, () => h(Child, null));      // pass the getter itself
h(For, { each: items }, item => h('li', null, () => item)); // same here
```

Lifecycle (`onMount`/`onCleanup`), effects, memos, `Show`/`For`/`Switch`/`Match`, event
props (`onClick`, … — attached with `addEventListener`, delegating `click` at `document`
exactly like upstream Solid), `style` objects and `classList` objects all behave as in the
browser, because they run against the real DOM.

## What the shim does

`src/shim.js` defines the handful of globals Solid touches that the Rendrix prelude
deliberately omits — currently just `SVGElement` (an `instanceof` guard in `solid-js/h`).
SVG tags fall back to plain elements via the prelude's `createElementNS`; real SVG
rendering needs engine support and is out of scope.

## Divergences and limits

- **Console noise on delegated clicks.** Solid redefines `currentTarget` as a getter-only
  accessor on the in-flight event; the prelude's strict-mode dispatch then throws `no setter
  for property` while cleaning up *after* every listener has run. Clicks work and state
  updates — the `[error] __rendrix_dispatch` line is benign noise. The engine-side fix is a
  one-liner in `prelude/events.js` (`event.currentTarget = null` → tolerant assignment);
  until then it is documented here, not hidden.
- **No SSR / hydration.** `renderToString`/`hydrate` exist in `solid-js/web` but there is no
  Rendrix host for them; only `render` is re-exported.
- **No TSX pragma.** `buildApp` pins `jsxImportSource` to `rendrix`, so `.tsx` always
  targets the built-in direct-DOM runtime. Use `h` (or call `jsx` from
  `rendrix-solid-adapter/jsx-runtime` directly) — see `src/jsx-runtime.js`.
- **Pinned version.** The adapter is verified against `solid-js@1.9.19` (exact pin). Newer
  1.x releases should work if they keep the same DOM surface; 2.x is untested.
