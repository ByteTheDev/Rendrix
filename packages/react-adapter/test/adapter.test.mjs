import test from 'node:test';
import assert from 'node:assert/strict';
import path from 'node:path';
import os from 'node:os';
import { spawnSync } from 'node:child_process';
import { mkdtemp, writeFile, rm } from 'node:fs/promises';
import { fileURLToPath } from 'node:url';
import { buildApp, writeBundle } from '../../cli/src/app/build.mjs';
import { findRuntime } from '../../cli/src/app/commands.mjs';
import { ensureVendor, haveReact, PINS } from '../scripts/vendor.mjs';

const here = path.dirname(fileURLToPath(import.meta.url));
const adapterRoot = path.resolve(here, '..');
const fixture = path.join(here, 'fixture');

async function buildFixture() {
  const { bundle, warnings } = await buildApp(fixture, { typecheck: false });
  for (const w of warnings) console.warn('build warning:', w);
  assert.ok(bundle.modules['src/main.js'], 'entry bundled');
  const keys = Object.keys(bundle.modules);
  // The ESM-ported React builds travel with the bundle (no bare imports at runtime).
  for (const k of ['vendor/react.js', 'vendor/scheduler.js', 'vendor/reconciler.js']) {
    assert.ok(keys.some(x => x.endsWith(k)), `vendored ${k} bundled, got: ${keys}`);
  }
  const dir = await mkdtemp(path.join(os.tmpdir(), 'rendrix-react-test-'));
  const bundlePath = path.join(dir, 'app.rdxapp');
  await writeBundle(bundle, bundlePath);
  return { dir, bundlePath };
}

// React commits sync-lane work on a microtask and passive effects (`useEffect`) via the
// scheduler, so the matrix adds two `wait` steps (3, 10) where Solid's synchronous
// reactivity asserts inline. Coverage mirrors Solid: mount, events, update, Suspense
// fallback/resolve, conditional unmount + cleanup, list update, root unmount.
const STEPS = [
  { type: 'expect', js: "document.getElementById('b').textContent + '|' + document.getElementById('sf').textContent", equals: 'n=0/0|loading' },
  // Passive effects flush via the scheduler and the lazy retry honors React's
  // fallback-throttle window, so this wait is longer than the one before the
  // effect-only assertion below.
  { type: 'wait', ms: 500 },
  { type: 'expect', js: "document.getElementById('lazy').textContent + '|' + log.join('|')", equals: 'lazy-hi|layout:0|mounted|effect:0' },
  { type: 'expect', js: "document.getElementById('child').className + '|' + document.getElementById('child').style.color + '|' + document.querySelectorAll('#list .it').length", equals: 'even|blue|2' },
  { type: 'click', selector: '#b' },
  { type: 'expect', js: "document.getElementById('b').textContent", equals: 'n=1/2' },
  { type: 'expect', js: "document.getElementById('child').className + '|' + document.getElementById('child').style.color + '|' + document.getElementById('jx').textContent + ',' + document.getElementById('jx2').textContent", equals: 'odd|red|jx1,jx21' },
  { type: 'eval', js: 'setShow(false)' },
  { type: 'expect', js: "document.getElementById('fb').textContent", equals: 'hidden' },
  { type: 'wait', ms: 200 },
  { type: 'expect', js: 'log.join("|")', equals: 'layout:0|mounted|effect:0|layout-cleaned|layout:1|effect-cleaned|effect:1|layout-cleaned|cleaned|effect-cleaned' },
  { type: 'eval', js: 'setItems(["x","y","z"])' },
  { type: 'expect', js: "document.querySelectorAll('#list .it').length", equals: '3' },
  { type: 'eval', js: 'dispose()' },
  { type: 'expect', js: "document.getElementById('root').childNodes.length", equals: '0' },
];

const SKIP = (!findRuntime() && 'runtime binary not built') || (!haveReact() && 'react not installed');

test('vendor ports expose the pinned React API', { skip: !haveReact() && 'react not installed' }, async () => {
  await ensureVendor();
  const [react, scheduler, constants, reconciler] = await Promise.all([
    import('../src/vendor/react.js'),
    import('../src/vendor/scheduler.js'),
    import('../src/vendor/constants.js'),
    import('../src/vendor/reconciler.js'),
  ]);
  assert.equal(react.version, PINS.react);
  for (const name of ['createElement', 'useState', 'useEffect', 'useLayoutEffect', 'useMemo', 'Suspense', 'lazy', 'Fragment']) {
    assert.ok(react[name] !== undefined, `react exports ${name}`);
  }
  assert.equal(typeof scheduler.unstable_scheduleCallback, 'function');
  assert.equal(constants.LegacyRoot, 0);
  assert.equal(typeof reconciler.default, 'function');
  const { default: createReconciler } = reconciler;
  assert.ok(createReconciler.length >= 1, 'factory takes the host config');
});

test('shim loads without a DOM and stays side-effect free', async () => {
  assert.equal(globalThis.SVGElement, undefined, 'precondition: no SVGElement in Node');
  await import('../src/shim.js');
  await import('../src/shim.js');
  assert.equal(globalThis.SVGElement, undefined, 'shim installs nothing today');
});

test('React component mounts, updates, events, cleanup and unmounts in the real runtime', { skip: SKIP }, async () => {
  await ensureVendor();
  const { dir, bundlePath } = await buildFixture();
  try {
    const stepsPath = path.join(dir, 'steps.json');
    await writeFile(stepsPath, JSON.stringify(STEPS));
    const r = spawnSync(findRuntime(), ['run', bundlePath, '--headless', '--script', stepsPath, '--size', '300x200'], { encoding: 'utf8' });
    assert.equal(r.status, 0, (r.stderr || '') + (r.stdout || ''));
    assert.match(r.stdout, /\[expect\] ok/);
  } finally { await rm(dir, { recursive: true, force: true }); }
});
