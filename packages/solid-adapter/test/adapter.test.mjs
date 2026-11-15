import test from 'node:test';
import assert from 'node:assert/strict';
import path from 'node:path';
import os from 'node:os';
import { spawnSync } from 'node:child_process';
import { mkdtemp, mkdir, writeFile, rm } from 'node:fs/promises';
import { fileURLToPath } from 'node:url';
import { buildApp, writeBundle } from '../../cli/src/app/build.mjs';
import { findRuntime } from '../../cli/src/app/commands.mjs';

const here = path.dirname(fileURLToPath(import.meta.url));
const adapterRoot = path.resolve(here, '..');
const fixture = path.join(here, 'fixture');

function haveSolid() {
  try {
    import.meta.resolve('solid-js', path.join(adapterRoot, 'src', 'index.js'));
    return true;
  } catch { return false; }
}

async function buildFixture() {
  const { bundle, warnings } = await buildApp(fixture, { typecheck: false });
  for (const w of warnings) console.warn('build warning:', w);
  assert.ok(bundle.modules['src/main.js'], 'entry bundled');
  const keys = Object.keys(bundle.modules);
  assert.ok(keys.some(k => k.includes('solid-js') && k.endsWith('solid.js')), `solid-js client bundled, got: ${keys}`);
  assert.ok(keys.some(k => k.includes('web.js')), 'solid-js/web bundled');
  assert.ok(keys.some(k => k.includes('h.js')), 'solid-js/h bundled');
  const dir = await mkdtemp(path.join(os.tmpdir(), 'rendrix-solid-test-'));
  const bundlePath = path.join(dir, 'app.rdxapp');
  await writeBundle(bundle, bundlePath);
  return { dir, bundlePath };
}

const STEPS = [
  { type: 'expect', js: "document.getElementById('b').textContent", equals: 'n=0/0' },
  { type: 'expect', js: 'log.join("|")', equals: 'mounted|effect:0' },
  { type: 'expect', js: "document.getElementById('child').className + '|' + document.getElementById('child').style.color", equals: 'even|blue' },
  { type: 'click', selector: '#b' },
  { type: 'expect', js: "document.getElementById('b').textContent", equals: 'n=1/2' },
  { type: 'expect', js: "document.getElementById('child').className + '|' + document.getElementById('child').style.color", equals: 'odd|red' },
  { type: 'expect', js: "document.getElementById('jx').textContent + ',' + document.getElementById('jx2').textContent", equals: 'jx1,jx21' },
  { type: 'eval', js: 'setShow(false)' },
  { type: 'expect', js: "document.getElementById('fb').textContent", equals: 'hidden' },
  { type: 'expect', js: 'log.join("|")', equals: 'mounted|effect:0|effect:1|cleaned' },
  { type: 'eval', js: 'setItems(["x","y","z"])' },
  { type: 'expect', js: "document.querySelectorAll('#list .it').length", equals: '3' },
  { type: 'eval', js: 'dispose()' },
  { type: 'expect', js: "document.getElementById('root').childNodes.length", equals: '0' },
];

test('shim installs SVGElement without a DOM', async () => {
  assert.equal(globalThis.SVGElement, undefined, 'precondition: no SVGElement in Node');
  await import('../src/shim.js');
  assert.ok(typeof globalThis.SVGElement === 'function', 'shim defines SVGElement');
  delete globalThis.SVGElement;
});

test('Solid component mounts, updates, events, cleanup and unmounts in the real runtime', { skip: (!findRuntime() && 'runtime binary not built') || (!haveSolid() && 'solid-js not installed') }, async () => {
  const { dir, bundlePath } = await buildFixture();
  try {
    const stepsPath = path.join(dir, 'steps.json');
    await writeFile(stepsPath, JSON.stringify(STEPS));
    const r = spawnSync(findRuntime(), ['run', bundlePath, '--headless', '--script', stepsPath, '--size', '300x200'], { encoding: 'utf8' });
    assert.equal(r.status, 0, (r.stderr || '') + (r.stdout || ''));
    assert.match(r.stdout, /\[expect\] ok/);
  } finally { await rm(dir, { recursive: true, force: true }); }
});

