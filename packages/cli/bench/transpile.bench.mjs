// M0 Node-side bench: `transpileModule` single-file cost + full `buildApp` cost.
// Dev-only harness, never shipped. Run: `node packages/cli/bench/transpile.bench.mjs`.
// Timing is informational in CI; use the medians to validate L1/L2/D1 build changes.

import { mkdtemp, mkdir, writeFile, rm } from 'node:fs/promises';
import os from 'node:os';
import path from 'node:path';
import ts from 'typescript';
import { buildApp } from '../src/app/build.mjs';

function median(v) {
  const s = [...v].sort((a, b) => a - b);
  return s[s.length >> 1];
}

async function timeIt(label, iters, fn) {
  const samples = [];
  for (let i = 0; i < iters; i++) {
    const t0 = performance.now();
    await fn(i);
    samples.push(performance.now() - t0);
  }
  console.log(`${label}: median ${median(samples).toFixed(2)} ms over ${iters} (min ${Math.min(...samples).toFixed(2)})`);
}

const singleTs = `import { add } from './util';\nexport interface Shape { n: number }\nexport const out: number = add(1, 2);\n`;

await timeIt('transpileModule/single 1KB TS file', 200, () => {
  ts.transpileModule(singleTs, {
    compilerOptions: { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.ESNext },
    fileName: 'single.ts',
  });
});

async function project(files) {
  const dir = await mkdtemp(path.join(os.tmpdir(), 'rendrix-transpile-bench-'));
  const all = { 'rendrix.json': '{"name":"bench","window":{"width":300,"height":200}}', 'index.html': '<html><body><script type="module" src="src/main.ts"></script></body></html>', ...files };
  for (const [name, content] of Object.entries(all)) {
    await mkdir(path.dirname(path.join(dir, name)), { recursive: true });
    await writeFile(path.join(dir, name), content);
  }
  return dir;
}

// 200-module project: one importer fanning out to 199 leaves (buildApp rewrite + transpile).
const leaves = {};
for (let i = 0; i < 199; i++) leaves[`src/leaf${i}.ts`] = `export const v${i}: number = ${i};\n`;
const imports = Array.from({ length: 199 }, (_, i) => `import { v${i} } from './leaf${i}';`).join('\n');
const big = await project({ 'src/main.ts': `${imports}\nexport const out: number = ${Array.from({ length: 199 }, (_, i) => `v${i}`).join(' + ')};`, ...leaves });
try {
  await timeIt('buildApp/200-module project (no typecheck)', 5, () => buildApp(big, { typecheck: false }));
  await timeIt('buildApp/200-module project (typecheck)', 3, () => buildApp(big, { typecheck: true }));
} finally {
  await rm(big, { recursive: true, force: true });
}
