#!/usr/bin/env node
// WPT subset runner: curated CSS layout cases (tables, floats, multi-column, plus
// grid/flexbox regression guards) executed in the real headless runtime. Results are
// tracked over time in `scripts/wpt-history.json` and rendered into `docs/WPT.md`.
//
// Usage: `node scripts/wpt.mjs [--update]` (default writes history + docs).
// Requires a built runtime (`npm run runtime` or RENDRIX_RUNTIME).

import { spawnSync } from 'node:child_process';
import path from 'node:path';
import os from 'node:os';
import { mkdtemp, mkdir, writeFile, readFile, rm } from 'node:fs/promises';
import { fileURLToPath } from 'node:url';
import { findRuntime } from '../packages/cli/src/app/commands.mjs';

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const historyPath = path.join(root, 'scripts', 'wpt-history.json');
const docsPath = path.join(root, 'docs', 'WPT.md');

/** Shorthand for a rounded rect field check. */
const rect = (id, field) => `Math.round(document.getElementById('${id}').getBoundingClientRect().${field})`;

const CASES = [
  {
    name: 'table-2x2-grid',
    css: 'body{margin:0} #t{width:300px} td{height:20px}',
    html: '<table id="t"><tr><td id="a">a</td><td id="b">b</td></tr><tr><td id="c">c</td><td id="d">d</td></tr></table>',
    checks: [
      { js: rect('b', 'x'), equals: '150' },
      { js: rect('a', 'width'), equals: '150' },
      { js: rect('c', 'y'), equals: '20' },
      { js: `(()=>{const a=document.getElementById('a').getBoundingClientRect(),b=document.getElementById('b').getBoundingClientRect();return Math.round(a.y)===Math.round(b.y)})()`, equals: 'true' },
    ],
  },
  {
    name: 'table-sections-caption-col',
    css: 'body{margin:0} #t{width:200px} td{height:10px}',
    html: '<table id="t"><caption id="cap">cap</caption><colgroup><col><col></colgroup><thead><tr><td id="h">h</td><td>h2</td></tr></thead><tbody><tr><td id="a">a</td><td id="b">b</td></tr></tbody></table>',
    checks: [
      { js: `(()=>{const c=document.getElementById('cap').getBoundingClientRect(),h=document.getElementById('h').getBoundingClientRect();return Math.round(h.y)>=Math.round(c.y+c.height)})()`, equals: 'true' },
      { js: `(()=>{const a=document.getElementById('a').getBoundingClientRect(),b=document.getElementById('b').getBoundingClientRect();return Math.round(a.y)===Math.round(b.y)})()`, equals: 'true' },
      { js: rect('b', 'x'), equals: '100' },
      { js: `getComputedStyle(document.querySelector('col')).display`, equals: 'table-column' },
    ],
  },
  {
    name: 'table-explicit-roles',
    css: 'body{margin:0} #t{display:table;width:200px} #r{display:table-row} #a,#b{display:table-cell;height:10px}',
    html: '<div id="t"><div id="r"><div id="a">a</div><div id="b">b</div></div></div>',
    checks: [
      { js: rect('a', 'width'), equals: '100' },
      { js: rect('b', 'x'), equals: '100' },
    ],
  },
  {
    name: 'table-ua-defaults',
    css: 'body{margin:0}',
    html: '<table id="t"><tr><td id="c">x</td></tr></table>',
    checks: [
      { js: `getComputedStyle(document.getElementById('t')).display`, equals: 'table' },
      { js: `getComputedStyle(document.querySelector('tr')).display`, equals: 'table-row' },
      { js: `getComputedStyle(document.getElementById('c')).display`, equals: 'table-cell' },
    ],
  },
  {
    name: 'float-left-pin',
    css: 'body{margin:0} #wrap{width:300px} #fl{float:left;width:100px;height:40px}',
    html: '<div id="wrap"><div id="fl">f</div></div>',
    checks: [
      { js: rect('fl', 'x'), equals: '0' },
      { js: rect('fl', 'width'), equals: '100' },
      { js: `getComputedStyle(document.getElementById('fl')).float`, equals: 'left' },
    ],
  },
  {
    name: 'float-right-pin',
    css: 'body{margin:0} #wrap{width:300px} #fr{float:right;width:120px;height:30px}',
    html: '<div id="wrap"><div id="fr">g</div></div>',
    checks: [
      { js: rect('fr', 'x'), equals: '180' },
      { js: rect('fr', 'width'), equals: '120' },
    ],
  },
  {
    name: 'clear-below-float',
    css: 'body{margin:0} #wrap{width:300px} #fl{float:left;width:100px;height:60px} #p{clear:left;height:20px;margin-top:-40px}',
    html: '<div id="wrap"><div id="fl">f</div><div id="p">p</div></div>',
    checks: [
      { js: rect('p', 'y'), equals: '60' },
      { js: `getComputedStyle(document.getElementById('p')).clear`, equals: 'left' },
    ],
  },
  {
    name: 'float-clear-computed',
    css: 'body{margin:0} #a{float:right} #b{clear:both}',
    html: '<div id="a">a</div><div id="b">b</div>',
    checks: [
      { js: `getComputedStyle(document.getElementById('a')).float`, equals: 'right' },
      { js: `getComputedStyle(document.getElementById('b')).clear`, equals: 'both' },
    ],
  },
  {
    name: 'multicol-count-2',
    css: 'body{margin:0} #mc{column-count:2;width:300px} #mc>div{height:10px}',
    html: '<div id="mc"><div id="a">a</div><div id="b">b</div><div id="c">c</div><div id="d">d</div></div>',
    checks: [
      { js: rect('a', 'width'), equals: '150' },
      { js: rect('b', 'x'), equals: '150' },
      { js: rect('c', 'y'), equals: '10' },
      { js: rect('d', 'x'), equals: '150' },
      { js: `getComputedStyle(document.getElementById('mc')).columnCount`, equals: '2' },
    ],
  },
  {
    name: 'multicol-span-all',
    css: 'body{margin:0} #mc{column-width:200px;width:800px} #mc>div{height:10px} #s{column-span:all}',
    html: '<div id="mc"><div id="a">a</div><div id="s">s</div><div id="b">b</div></div>',
    checks: [
      { js: rect('a', 'width'), equals: '200' },
      { js: rect('s', 'width'), equals: '800' },
    ],
  },
  {
    name: 'multicol-columns-shorthand',
    css: 'body{margin:0} #mc{columns:150px 2;width:300px} #mc>div{height:10px}',
    html: '<div id="mc"><div id="a">a</div><div id="b">b</div></div>',
    checks: [
      { js: rect('b', 'x'), equals: '150' },
      { js: `getComputedStyle(document.getElementById('mc')).columnCount`, equals: '2' },
    ],
  },
  {
    name: 'guard-flex-row',
    css: 'body{margin:0} #row{display:flex;width:400px;height:100px;gap:10px;padding:10px} #a{width:80px} #b{flex:1} #c{width:60px;height:30px}',
    html: '<div id="row"><div id="a"></div><div id="b"></div><div id="c"></div></div>',
    checks: [
      { js: rect('a', 'width'), equals: '80' },
      { js: rect('b', 'x'), equals: '100' },
    ],
  },
  {
    name: 'guard-grid-columns',
    css: 'body{margin:0} #g{display:grid;grid-template-columns:100px 1fr;gap:10px;width:310px} #g>div{height:20px}',
    html: '<div id="g"><div id="a"></div><div id="b"></div></div>',
    checks: [
      { js: rect('a', 'width'), equals: '100' },
      { js: rect('b', 'x'), equals: '110' },
    ],
  },
  {
    name: 'guard-block-percent-padding',
    css: 'body{margin:0} #w{width:400px;padding:10px} #k{height:30px;width:50%}',
    html: '<div id="w"><div id="k"></div></div>',
    checks: [
      { js: rect('k', 'width'), equals: '200' },
      { js: rect('k', 'x'), equals: '10' },
    ],
  },
  {
    name: 'guard-absolute-position',
    css: 'body{margin:0} #host{position:relative;width:300px;height:200px} #abs{position:absolute;right:10px;bottom:10px;width:50px;height:50px}',
    html: '<div id="host"><div id="abs"></div></div>',
    checks: [
      { js: rect('abs', 'x'), equals: '240' },
      { js: rect('abs', 'y'), equals: '140' },
    ],
  },
];

async function runCase(runtime, c) {
  const dir = await mkdtemp(path.join(os.tmpdir(), 'rendrix-wpt-'));
  try {
    const html = `<html><head><style>${c.css}</style></head><body>${c.html}</body></html>`;
    const bundle = {
      format: 'rendrix-app/1', name: 'wpt', html, modules: {}, assets: {},
      permissions: { network: false, fs_read: [], fs_write: [] },
    };
    const bundlePath = path.join(dir, 'case.rdxapp');
    await writeFile(bundlePath, JSON.stringify(bundle));
    const steps = c.checks.map(ch => ({ type: 'expect', js: ch.js, equals: ch.equals }));
    const stepsPath = path.join(dir, 'steps.json');
    await writeFile(stepsPath, JSON.stringify(steps));
    const r = spawnSync(runtime, ['run', bundlePath, '--headless', '--script', stepsPath, '--size', '800x600'], { encoding: 'utf8' });
    const oks = (r.stdout.match(/\[expect\] ok/g) || []).length;
    if (r.status === 0 && oks === c.checks.length) return { name: c.name, pass: true, detail: `${oks}/${c.checks.length}` };
    const tail = (r.stderr + '\n' + r.stdout).trim().split('\n').slice(-4).join(' | ').slice(0, 300);
    return { name: c.name, pass: false, detail: `${oks}/${c.checks.length}: ${tail}` };
  } finally {
    await rm(dir, { recursive: true, force: true });
  }
}

function docFor(history, latest) {
  const rate = latest.total ? ((100 * latest.pass / latest.total).toFixed(1) + '%') : 'n/a';
  const rows = latest.cases.map(c => `| ${c.pass ? '✅' : '❌'} | ${c.name} | ${c.detail} |`).join('\n');
  const hist = history.slice(-12).reverse()
    .map(h => `| ${h.date} | ${h.pass}/${h.total} (${(100 * h.pass / (h.total || 1)).toFixed(1)}%) |`).join('\n');
  return `# WPT subset conformance

Curated CSS layout subset run against the real headless runtime
(\`node scripts/wpt.mjs\`). It covers the layout areas this project implements
(tables, floats, multi-column, grid, flexbox, block flow, positioning) — it is **not**
the full web-platform-tests suite, and the rate below tracks *this subset only*.
No browser parity is claimed.

Run it: \`node scripts/wpt.mjs\` (needs a built runtime: \`npm run runtime\`).
History: \`scripts/wpt-history.json\` (appended per run, last 50 kept).

## Latest (${latest.date}): ${latest.pass}/${latest.total} (${rate})

|  | Case | Checks |
|---|---|---|
${rows}

## Pass rate over time (last 12 runs)

| Date | Pass |
|---|---|
${hist}
`;
}

async function main() {
  const runtime = findRuntime();
  if (!runtime) {
    console.error('wpt: no runtime binary found. Build it (`npm run runtime`) or set RENDRIX_RUNTIME.');
    process.exit(2);
  }
  const results = [];
  for (const c of CASES) {
    const r = await runCase(runtime, c);
    results.push(r);
    console.log(`${r.pass ? 'PASS' : 'FAIL'} ${r.name} (${r.detail})`);
  }
  const pass = results.filter(r => r.pass).length;
  const latest = { date: new Date().toISOString().slice(0, 10), pass, total: results.length, cases: results };
  let history = [];
  try { history = JSON.parse(await readFile(historyPath, 'utf8')); } catch { /* first run */ }
  if (!Array.isArray(history)) history = [];
  history.push(latest);
  await mkdir(path.dirname(historyPath), { recursive: true });
  await writeFile(historyPath, JSON.stringify(history.slice(-50), null, 2) + '\n');
  await writeFile(docsPath, docFor(history, latest));
  console.log(`\nwpt subset: ${pass}/${results.length} pass — history → scripts/wpt-history.json, report → docs/WPT.md`);
  process.exit(pass === results.length ? 0 : 1);
}

main().catch(e => { console.error(e.stack || String(e)); process.exit(1); });
