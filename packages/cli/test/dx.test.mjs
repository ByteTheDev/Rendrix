import test from 'node:test';
import assert from 'node:assert/strict';
import path from 'node:path';
import os from 'node:os';
import { mkdtemp, mkdir, writeFile, readFile, rm } from 'node:fs/promises';
import { buildApp } from '../src/app/build.mjs';
import { diffModules, writeHotUpdate } from '../src/app/commands.mjs';

async function project(files) {
  const dir = await mkdtemp(path.join(os.tmpdir(), 'rendrix-dx-test-'));
  const all = { 'rendrix.json': '{"name":"t","window":{"width":300,"height":200}}', 'index.html': '<html><body><div id=root></div><script type="module" src="src/main.ts"></script></body></html>', ...files };
  for (const [name, content] of Object.entries(all)) {
    await mkdir(path.dirname(path.join(dir, name)), { recursive: true });
    await writeFile(path.join(dir, name), content);
  }
  return dir;
}
const cleanup = dir => rm(dir, { recursive: true, force: true });

test('diffModules finds changed, added and removed bundle keys', () => {
  const oldM = { 'a.js': '1', 'b.js': '2', 'gone.js': 'x' };
  const newM = { 'a.js': '1', 'b.js': '3', 'c.js': 'new' };
  assert.deepEqual(diffModules(oldM, newM), { changed: ['b.js', 'c.js'], removed: ['gone.js'] });
  assert.deepEqual(diffModules(oldM, oldM), { changed: [], removed: [] });
  assert.deepEqual(diffModules({}, {}), { changed: [], removed: [] });
});

test('writeHotUpdate writes the full module set as rendrix-hot/1', async () => {
  const dir = await mkdtemp(path.join(os.tmpdir(), 'rendrix-hot-test-'));
  try {
    const file = await writeHotUpdate(dir, { modules: { 'main.js': 'code' } });
    assert.equal(file, path.join(dir, '.rendrix', 'hot-update.json'));
    const sidecar = JSON.parse(await readFile(file, 'utf8'));
    assert.equal(sidecar.format, 'rendrix-hot/1');
    assert.deepEqual(sidecar.modules, { 'main.js': 'code' });
    assert.ok(typeof sidecar.at === 'number');
  } finally { await cleanup(dir); }
});

test('edit → rebuild → diff detects exactly the changed modules (dev-loop incremental path)', async () => {
  const dir = await project({
    'src/main.ts': "import { label } from './lib';\nimport './theme.css';\ndocument.getElementById('root')!.textContent = label;",
    'src/lib.ts': "export const label = 'v1';",
    'src/theme.css': '#root{color:rgb(0, 0, 0)}',
  });
  try {
    const first = await buildApp(dir, { typecheck: false });
    assert.ok(first.bundle.modules['src/theme.css.js'], 'imported CSS becomes a JS module');
    await writeFile(path.join(dir, 'src/lib.ts'), "export const label = 'v2';");
    const second = await buildApp(dir, { typecheck: false });
    const { changed, removed } = diffModules(first.bundle.modules, second.bundle.modules);
    assert.deepEqual(changed, ['src/lib.js'], `only the edited module differs, got ${changed}`);
    assert.deepEqual(removed, []);
    const sidecar = await writeHotUpdate(dir, second.bundle);
    assert.ok((await readFile(sidecar, 'utf8')).includes('v2'));
    // An unrelated edit does not touch other modules: byte-identical output.
    assert.equal(first.bundle.modules['src/main.js'], second.bundle.modules['src/main.js']);
  } finally { await cleanup(dir); }
});
