import { createSignal, createEffect, createMemo, onMount, onCleanup, Show, For, render, h } from '../../../src/index.js';
import { jsx, jsxs } from '../../../src/jsx-runtime.js';

const [count, setCount] = createSignal(0);
const doubled = createMemo(() => count() * 2);
globalThis.setCount = setCount;
globalThis.log = [];
let cleanupRan = false;

function Child() {
  onMount(() => globalThis.log.push('mounted'));
  onCleanup(() => { cleanupRan = true; globalThis.log.push('cleaned'); });
  createEffect(() => globalThis.log.push('effect:' + count()));
  return h('span', { id: 'child', class: () => (count() % 2 === 1 ? 'odd' : 'even'), style: () => ({ color: count() % 2 === 1 ? 'red' : 'blue' }) }, () => 'c=' + count());
}

function App() {
  const [show, setShow] = createSignal(true);
  globalThis.setShow = setShow;
  const [items, setItems] = createSignal(['a', 'b']);
  globalThis.setItems = setItems;
  return h('div', { id: 'app' },
    h('button', { id: 'b', onClick: () => setCount(c => c + 1) }, () => 'n=' + count() + '/' + doubled()),
    h(Show, { when: show, fallback: h('i', { id: 'fb' }, 'hidden') }, () => h(Child, null)),
    h('ul', { id: 'list' }, h(For, { each: items }, item => h('li', { class: 'it' }, () => item))),
    jsx('p', { id: 'jx', children: () => 'jx' + count() }),
    jsxs('p', { id: 'jx2', children: () => 'jx2' + count() }));
}

const dispose = render(App, document.getElementById('root'));
globalThis.dispose = dispose;
globalThis.wasCleaned = () => cleanupRan;
