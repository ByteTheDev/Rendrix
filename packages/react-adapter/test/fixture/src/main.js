import { useState, useMemo, useEffect, useLayoutEffect, onMount, onCleanup, render, flushSync, h, Suspense, lazy } from '../../../src/index.js';
import { jsx, jsxs } from '../../../src/jsx-runtime.js';

globalThis.log = [];

function Child({ n }) {
  onMount(() => globalThis.log.push('mounted'));
  onCleanup(() => globalThis.log.push('cleaned'));
  useLayoutEffect(() => {
    globalThis.log.push('layout:' + n);
    return () => globalThis.log.push('layout-cleaned');
  }, [n]);
  useEffect(() => {
    globalThis.log.push('effect:' + n);
    return () => globalThis.log.push('effect-cleaned');
  }, [n]);
  return h('span', { id: 'child', className: n % 2 === 1 ? 'odd' : 'even', style: { color: n % 2 === 1 ? 'red' : 'blue' } }, 'c=' + n);
}

// A lazily-loaded leaf: resolves on a microtask, so the first commit shows the
// Suspense fallback and a later frame shows the content.
const LazyHi = lazy(() => Promise.resolve({ default: function Hi() { return h('b', { id: 'lazy' }, 'lazy-hi'); } }));

function App() {
  const [count, setCount] = useState(0);
  const doubled = useMemo(() => count * 2, [count]);
  const [show, setShow] = useState(true);
  const [items, setItems] = useState(['a', 'b']);
  globalThis.setCount = v => flushSync(() => setCount(v));
  globalThis.setShow = v => flushSync(() => setShow(v));
  globalThis.setItems = v => flushSync(() => setItems(v));
  return h('div', { id: 'app' },
    h('button', { id: 'b', onClick: () => setCount(c => c + 1) }, 'n=' + count + '/' + doubled),
    show ? h(Child, { n: count }) : h('i', { id: 'fb' }, 'hidden'),
    h('ul', { id: 'list' }, items.map(item => h('li', { className: 'it', key: item }, item))),
    jsx('p', { id: 'jx', children: 'jx' + count }),
    jsxs('p', { id: 'jx2', children: 'jx2' + count }),
    h(Suspense, { fallback: h('i', { id: 'sf' }, 'loading') }, h(LazyHi, null)));
}

const unmount = render(h(App, null), document.getElementById('root'));
globalThis.dispose = () => unmount();
