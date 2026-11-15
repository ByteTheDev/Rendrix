import test from 'node:test';
import assert from 'node:assert/strict';
import path from 'node:path';
import os from 'node:os';
import { mkdtemp, mkdir, writeFile, readFile, rm } from 'node:fs/promises';
import { buildApp } from '../src/app/build.mjs';
import { diffModules, writeHotUpdate, renderTarget, render, findRuntime } from '../src/app/commands.mjs';

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

test('renderTarget classifies -o by extension', () => {
  assert.deepEqual(renderTarget('shot.png'), { kind: 'png' });
  assert.deepEqual(renderTarget('SHOT.PNG'), { kind: 'png' });
  assert.deepEqual(renderTarget('doc.pdf'), { kind: 'pdf' });
  assert.deepEqual(renderTarget('frames/'), { kind: 'frames' });
  assert.deepEqual(renderTarget('frames'), { kind: 'frames' });
});

test('render validates input and flags before touching the runtime', async () => {
  await assert.rejects(render('/definitely/not/a-page.html', { out: 'x.png' }), /does not exist/);
  const dir = await mkdtemp(path.join(os.tmpdir(), 'rendrix-render-arg-test-'));
  try {
    await writeFile(path.join(dir, 'note.txt'), 'not html');
    await assert.rejects(render(path.join(dir, 'note.txt'), { out: 'x.png' }), /not an HTML file/);
    await writeFile(path.join(dir, 'page.html'), '<html><body>hi</body></html>');
    await assert.rejects(render(path.join(dir, 'page.html'), { out: 'x.png', size: 'bogus' }), /--size/);
    await assert.rejects(render(path.join(dir, 'page.html'), { out: 'x.png', scale: 9 }), /--scale/);
    await assert.rejects(render(path.join(dir, 'page.html'), { out: 'clip.mp4' }), /\/video/);
  } finally { await cleanup(dir); }
});

test('render stages the page plus its references, not unrelated siblings', { skip: !findRuntime() && 'runtime binary not built' }, async () => {
  // An unrelated multi-megabyte sibling next to the page must not break (or bloat) the render.
  const dir = await mkdtemp(path.join(os.tmpdir(), 'rendrix-render-stage-test-'));
  try {
    await writeFile(path.join(dir, 'page.html'), '<html><head><link rel="stylesheet" href="a.css"></head><body><div id="t">staged</div><script type="module" src="app.js"></script></body></html>');
    await writeFile(path.join(dir, 'a.css'), 'body{margin:0} #t{width:50px;height:10px}');
    await writeFile(path.join(dir, 'app.js'), "import { x } from './lib.js'; document.getElementById('t').textContent = x;");
    await writeFile(path.join(dir, 'lib.js'), "export const x = 'staged';");
    await writeFile(path.join(dir, 'unrelated.bin'), Buffer.alloc(9 * 1024 * 1024, 7));
    const out = path.join(dir, 'shot.png');
    const r = await render(path.join(dir, 'page.html'), { out, size: '200x100' });
    assert.equal(r.kind, 'png');
    const bytes = await readFile(out);
    assert.ok(bytes[0] === 0x89, 'a real PNG screenshot (scripts and CSS staged and built)');
  } finally { await cleanup(dir); }
});

const renderPage = '<html><head><style>body{margin:0} table{width:300px} td{height:20px} .fl{float:left;width:100px;height:40px} #mc{column-count:2} #mc>div{height:10px}</style></head><body><table><tr><td>a</td><td>b</td></tr></table><div class="fl">f</div><div id="mc"><div>a</div><div>b</div></div></body></html>';

test('render produces a PNG headlessly', { skip: !findRuntime() && 'runtime binary not built' }, async () => {
  const dir = await mkdtemp(path.join(os.tmpdir(), 'rendrix-render-png-test-'));
  try {
    await writeFile(path.join(dir, 'page.html'), renderPage);
    const out = path.join(dir, 'shot.png');
    const r = await render(path.join(dir, 'page.html'), { out, size: '400x300' });
    assert.equal(r.kind, 'png');
    const bytes = await readFile(out);
    assert.ok(bytes.length > 100 && bytes[0] === 0x89 && bytes.subarray(1, 4).toString() === 'PNG', 'a real PNG screenshot');
  } finally { await cleanup(dir); }
});

test('render produces a one-page PDF', { skip: !findRuntime() && 'runtime binary not built' }, async () => {
  const dir = await mkdtemp(path.join(os.tmpdir(), 'rendrix-render-pdf-test-'));
  try {
    await writeFile(path.join(dir, 'page.html'), renderPage);
    const out = path.join(dir, 'shot.pdf');
    const r = await render(path.join(dir, 'page.html'), { out, size: '400x300' });
    assert.equal(r.kind, 'pdf');
    const head = (await readFile(out, 'utf8')).slice(0, 8);
    assert.equal(head, '%PDF-1.4');
  } finally { await cleanup(dir); }
});

test('render produces a PNG frame sequence', { skip: !findRuntime() && 'runtime binary not built' }, async () => {
  const dir = await mkdtemp(path.join(os.tmpdir(), 'rendrix-render-frames-test-'));
  try {
    await writeFile(path.join(dir, 'page.html'), renderPage);
    const out = path.join(dir, 'frames');
    const r = await render(path.join(dir, 'page.html'), { out, size: '320x240', frames: 2, frameMs: 0 });
    assert.equal(r.kind, 'frames');
    for (const f of ['frame-0001.png', 'frame-0002.png']) {
      const bytes = await readFile(path.join(out, f));
      assert.ok(bytes[0] === 0x89, `${f} is a PNG`);
    }
  } finally { await cleanup(dir); }
});
