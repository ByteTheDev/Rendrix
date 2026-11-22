// `rendrix` commands for application projects: init, check, build, run, dev, package, doctor, render.

import os from 'node:os';
import path from 'node:path';
import { spawn, spawnSync } from 'node:child_process';
import { existsSync, watch, statSync } from 'node:fs';
import { readFile, writeFile, mkdir, mkdtemp, readdir, copyFile, rm, stat } from 'node:fs/promises';
import { fileURLToPath } from 'node:url';
import { buildApp, writeBundle, bundleStats, typeCheck, loadProject, BuildError, BUNDLE_FORMAT } from './build.mjs';
import { resolveTarget, currentTarget, isCrossTarget, supportedTargets } from './target.mjs';
import { packExecutable, MAGIC, writeMacApp, zipDirectory, writeLinuxRoot, tarDirectory } from './packaging.mjs';
import { resolveIcon, icnsFromIcon, pngFromIcon } from './icon.mjs';
import { writeManifest, resolveSignCommand, signArtifact, notarizeArtifact, findSigntool, findCodesign } from '../signing.mjs';
import { buildNsis } from '../installer/nsis.mjs';

const here = path.dirname(fileURLToPath(import.meta.url));
const cliRoot = path.resolve(here, '..', '..');
const repoRoot = path.resolve(cliRoot, '..', '..');

// Keep the original public imports working for consumers of this module.
export { MAGIC, packExecutable };

const color = (c, s) => (process.stdout.isTTY ? `\x1b[${c}m${s}\x1b[0m` : s);
const ok = s => console.log(color(32, '✓'), s);
const info = s => console.log(color(36, '›'), s);
const warn = s => console.warn(color(33, '!'), s);
const fail = s => console.error(color(31, '✗'), s);

/**
 * Locate a prebuilt runtime for `target`. Local `engine/target` builds are host-only: when
 * cross-packaging we must not silently embed a binary for the wrong OS/architecture, so only
 * an explicit RENDRIX_RUNTIME / --runtime path or a RENDRIX_HOME distribution is considered.
 */
export function findRuntime(target = currentTarget()) {
  const binary = 'rendrix-runtime' + target.exe;
  const cross = isCrossTarget(target);
  const candidates = [
    process.env.RENDRIX_RUNTIME,
    !cross && path.join(repoRoot, 'engine', 'target', 'release', binary),
    !cross && path.join(repoRoot, 'engine', 'target', 'debug', binary),
    process.env.RENDRIX_HOME && path.join(process.env.RENDRIX_HOME, 'runtime', target.key, binary),
  ].filter(Boolean);
  return candidates.find(p => existsSync(p)) || null;
}

function needRuntime(target) {
  const rt = findRuntime(target);
  if (!rt) {
    const hint = target && isCrossTarget(target)
      ? `No runtime for ${target.key} was found. Set RENDRIX_HOME to a distribution containing runtime/${target.key}/, or pass --runtime /path/to/rendrix-runtime${target.exe}. Local engine/target builds are host-only.`
      : 'The Rendrix runtime is not built. Run `cargo build --release -p rendrix-runtime --manifest-path engine/Cargo.toml`, or set RENDRIX_RUNTIME to a rendrix-runtime executable. `rendrix doctor` shows what was searched.';
    throw new BuildError(hint);
  }
  return rt;
}

export async function build(dir, opts = {}) {
  const t0 = Date.now();
  const { bundle, project, warnings, sources } = await buildApp(dir, { typecheck: opts.typecheck !== false, checkJs: opts.checkJs === true });
  for (const w of warnings) warn(w);
  const out = path.resolve(opts.out || path.join(project.dir, 'dist', `${project.name}.rdxapp`));
  await writeBundle(bundle, out);
  const s = bundleStats(bundle);
  ok(`built ${project.name}: ${s.modules} module${s.modules === 1 ? '' : 's'} (${(s.moduleBytes / 1024).toFixed(1)} KB JS), ${s.assets} asset${s.assets === 1 ? '' : 's'} in ${Date.now() - t0} ms → ${path.relative(process.cwd(), out)}`);
  return { out, bundle, project, sources };
}

export async function check(dir, opts = {}) {
  const project = await loadProject(dir);
  const errors = await typeCheck(project, { checkJs: opts.checkJs === true });
  if (errors.length) throw new BuildError(`${errors.length} type error${errors.length === 1 ? '' : 's'}:\n${errors.join('\n')}`, errors);
  ok(`${project.name}: no type errors`);
}

export async function run(dir, opts = {}) {
  const { out } = await build(dir, opts);
  const rt = needRuntime();
  const args = ['run', out, ...(opts.runtimeArgs || [])];
  const child = spawnSync(rt, args, { stdio: 'inherit' });
  return child.status ?? 1;
}

export async function pack(dir, opts = {}) {
  const target = resolveTarget(opts.target);
  const { bundle, project } = await build(dir, { ...opts, out: opts.bundleOut });
  const rt = opts.runtime || needRuntime(target);
  const version = await appVersion(project.dir, opts);
  const icon = await resolveIcon(project.dir, opts.icon);
  const dist = path.join(project.dir, 'dist');

  if (target.platform === 'darwin') {
    // A complete .app bundle with Resources, icon and metadata, zipped for distribution and
    // notarization. The binary is signed inside the bundle before the archive is created.
    const appDir = path.resolve(opts.out || path.join(dist, `${project.name}.app`));
    if (!appDir.endsWith('.app')) throw new BuildError(`--out for macOS must name a .app bundle (got '${appDir}')`);
    if (opts.installer && !['none', 'zip'].includes(opts.installer)) {
      throw new BuildError(`installer '${opts.installer}' is not supported on macOS; the .app is zipped automatically`);
    }
    const { executable } = await writeMacApp({
      appDir, project, version, runtime: rt, bundle, target, icon,
      icns: icnsFromIcon(icon, warn),
    });
    const zipPath = path.join(path.dirname(appDir), `${project.name}-${version}-${target.key}.zip`);
    // --keepParent semantics: notarytool expects the .app folder itself inside the zip.
    await writeFile(zipPath, await zipDirectory(appDir, path.basename(appDir)));
    ok(`packaged ${path.relative(process.cwd(), appDir)} (${(statSync(executable).size / 1048576).toFixed(1)} MB standalone binary, zipped for distribution)`);
    return finishPack({
      project, version, out: appDir, artifacts: [zipPath], signFiles: [appDir],
      notarize: { app: appDir, zip: zipPath },
    }, opts);
  }

  if (target.platform === 'linux') {
    if (opts.installer && !['none', 'tarball'].includes(opts.installer)) {
      throw new BuildError(`installer '${opts.installer}' is not supported on Linux (the tarball is the distributable)`);
    }
    const rootName = `${project.name}-${version}-${target.key}`;
    const root = path.join(dist, rootName);
    await rm(root, { recursive: true, force: true });
    const { executable } = await writeLinuxRoot({
      root, project, version, runtime: rt, bundle, target,
      png: pngFromIcon(icon, warn),
    });
    const tarPath = path.resolve(opts.out || path.join(dist, `${rootName}.tar.gz`));
    await writeFile(tarPath, await tarDirectory(root));
    ok(`packaged ${path.relative(process.cwd(), tarPath)} (${(statSync(executable).size / 1048576).toFixed(1)} MB standalone binary + desktop entry)`);
    return finishPack({ project, version, out: executable, artifacts: [tarPath], signFiles: [tarPath] }, opts);
  }

  // Windows
  if (opts.installer && !['none', 'nsis'].includes(opts.installer)) {
    throw new BuildError(`unknown installer '${opts.installer}' (supported: none, nsis)`);
  }
  const out = path.resolve(opts.out || path.join(dist, project.name + target.exe));
  await packExecutable(rt, bundle, out, target);
  const artifacts = [out];
  ok(`packaged ${path.relative(process.cwd(), out)} (${(statSync(out).size / 1048576).toFixed(1)} MB, standalone; needs no Node, Bun or browser)`);
  let installer = null;
  if (opts.installer === 'nsis') {
    if (process.platform !== 'win32') throw new BuildError('building an NSIS installer requires Windows (makensis); cross-packaging the plain executable is supported');
    const scriptPath = path.join(path.dirname(out), `${project.name}.nsi`);
    const setupOut = path.join(path.dirname(out), `${project.name}-setup-${version}.exe`);
    info(`building NSIS installer → ${path.relative(process.cwd(), setupOut)}`);
    await buildNsis({ name: project.name, version, executable: out, output: setupOut, publishUrl: opts.publishUrl || '', certificateSubjectName: process.env.RENDRIX_CERT_SUBJECT || '', arch: target.arch }, scriptPath);
    installer = setupOut;
    artifacts.push(installer);
  }
  return finishPack({ project, version, artifacts, signFiles: [out, installer].filter(Boolean), out, installer }, opts);
}

async function appVersion(projectDir, opts) {
  if (opts.appVersion) return String(opts.appVersion);
  if (process.env.RENDRIX_APP_VERSION) return String(process.env.RENDRIX_APP_VERSION);
  try {
    const raw = JSON.parse(await readFile(path.join(projectDir, 'rendrix.json'), 'utf8'));
    if (typeof raw.version === 'string' && raw.version) return raw.version;
  } catch { /* optional */ }
  return '0.0.0';
}

/// Sign artifacts, write the SHA-256 manifest (+ detached `.sig` with `--manifest-key`), and
/// notarize the zipped .app on macOS.
async function finishPack(pack, opts) {
  if (opts.sign) {
    const command = typeof opts.sign === 'string' ? opts.sign : resolveSignCommand();
    if (!command) throw new BuildError('signing requested but no signing command is configured (set RENDRIX_SIGN_COMMAND, or RENDRIX_CERT_FILE on Windows / RENDRIX_CODESIGN_IDENTITY on macOS)');
    for (const file of pack.signFiles || pack.artifacts) {
      info(`signing ${path.basename(file)}`);
      await signArtifact(file, command);
    }
    if (pack.notarize && process.env.APPLE_ID) {
      await notarizeArtifact(pack.notarize.zip, { app: pack.notarize.app, zip: pack.notarize.zip });
    }
  }
  if (opts.manifest) {
    const manifestPath = path.resolve(opts.manifest);
    await writeManifest(pack.artifacts, manifestPath, opts.manifestKey);
    ok(`wrote update manifest ${path.relative(process.cwd(), manifestPath)}${opts.manifestKey ? ' + .sig' : ''}`);
    if (opts.publishUrl) info(`publish ${pack.artifacts.map(f => path.basename(f)).join(', ')} and ${path.basename(manifestPath)} to ${opts.publishUrl}, then point the app at it with rendrix.update.check(feedUrl)`);
  } else if (opts.publishUrl) {
    warn('--publish-url without --manifest only records the URL in the installer script; pass --manifest to emit the file');
  }
  return pack.out;
}

export async function dev(dir, opts = {}) {
  const rt = needRuntime();
  let child = null;
  let building = false, again = false, watched = new Set();
  // D2 incremental path: the last good bundle, diffed against each rebuild so the runtime can
  // hot-swap instead of restarting (see `diffModules`/`writeHotUpdate` below and the D2 report
  // for the `hot_reload::apply_module_update` wiring on the runtime side).
  let lastBundle = null;
  const watchers = [];
  const start = out => {
    if (child) { child.removeAllListeners('exit'); child.kill(); }
    child = spawn(rt, ['run', out, ...(opts.runtimeArgs || [])], { stdio: 'inherit' });
    child.on('exit', code => { if (code !== null) { info(`app exited (${code}); press Ctrl+C to stop watching`); child = null; } });
  };
  // D2 hot path: start the app once with the sidecar path; rebuilds only rewrite the file
  // (the runtime polls its mtime in about_to_wait and calls apply_module_update — no respawn):
  const startHot = (out, sidecar) => {
    if (child) { child.removeAllListeners('exit'); child.kill(); }
    child = spawn(rt, ['run', out, ...(opts.runtimeArgs || [])], { stdio: 'inherit', env: { ...process.env, RENDRIX_HOT_FILE: sidecar } });
    child.on('exit', code => { if (code !== null) { info(`app exited (${code}); press Ctrl+C to stop watching`); child = null; } });
  };
  async function rebuild(first) {
    if (building) { again = true; return; }
    building = true;
    try {
      const { out, sources, bundle } = await build(dir, opts);
      if (lastBundle) {
        const { changed, removed } = diffModules(lastBundle.modules, bundle.modules);
        const sidecar = await writeHotUpdate(dir, bundle);
        const names = [...changed.map(k => `~${k}`), ...removed.map(k => `-${k}`)].slice(0, 5).join(' ');
        info(`hot: ${changed.length} changed, ${removed.length} removed${names ? ` (${names})` : ''} → ${path.relative(process.cwd(), sidecar)}`);
        if (opts.hot === false) {
          info('hot: disabled via --no-hot; restarting the app');
          start(out);
        } else if (!child) {
          startHot(out, sidecar);
        } else if (changed.length || removed.length) {
          info('hot: running app picks up the sidecar without a restart');
        } else {
          info('hot: modules identical; app keeps running');
        }
      } else {
        // First build: start with the sidecar so later rebuilds hot-swap without respawn.
        const sidecar = await writeHotUpdate(dir, bundle);
        if (opts.hot === false) start(out); else startHot(out, sidecar);
      }
      lastBundle = bundle;
      sources.forEach(s => watched.add(path.dirname(s)));
      if (first) attachWatchers();
    } catch (e) {
      fail(e instanceof BuildError ? e.message : e.stack || String(e));
      if (first && !watchers.length) { watched.add(path.resolve(dir)); attachWatchers(); }
      info('waiting for changes… (the last working build keeps running)');
    }
    building = false;
    if (again) { again = false; rebuild(false); }
  }
  let timer;
  function attachWatchers() {
    const root = path.resolve(dir);
    try {
      watchers.push(watch(root, { recursive: true }, (_ev, file) => {
        if (!file || /(^|[\\/])(node_modules|dist|\.git|\.rendrix|target)([\\/]|$)/.test(file)) return;
        clearTimeout(timer);
        timer = setTimeout(() => { info(`change in ${file}`); rebuild(false); }, 120);
      }));
    } catch (e) { warn(`file watching unavailable: ${e.message}`); }
  }
  await rebuild(true);
  info('watching for changes (Ctrl+C to stop)');
  await new Promise(resolve => { process.on('SIGINT', () => { watchers.forEach(w => w.close()); if (child) child.kill(); resolve(); }); });
}

// ----- hot reload sidecar (D2) ------------------------------------------------------------------
//
// `dev` rebuilds with `buildApp` (the whole project; per-file incremental compile is future
// work) and diffs the emitted `bundle.modules` maps to find what changed. The full post-update
// module set is written to `.rendrix/hot-update.json` (`rendrix-hot/1`), which a hot-swap-aware
// runtime consumes in-process via `hot_reload::apply_module_update` (wiring snippet in the D2
// report) instead of restarting. The `.rendrix` directory is excluded from the watcher above so
// sidecar writes never retrigger a rebuild.

/** Keys whose emitted JS differs between two `bundle.modules` maps, plus removals. */
export function diffModules(oldModules, newModules) {
  const changed = [], removed = [];
  for (const [k, v] of Object.entries(newModules || {})) if ((oldModules || {})[k] !== v) changed.push(k);
  for (const k of Object.keys(oldModules || {})) if (!(k in (newModules || {}))) removed.push(k);
  return { changed, removed };
}

/** Write the full updated module set for the runtime to hot-swap. Returns the sidecar path. */
export async function writeHotUpdate(dir, bundle) {
  const file = path.join(path.resolve(dir), '.rendrix', 'hot-update.json');
  await mkdir(path.dirname(file), { recursive: true });
  await writeFile(file, JSON.stringify({ format: 'rendrix-hot/1', at: Date.now(), modules: bundle.modules }));
  return file;
}

// ----- init -----------------------------------------------------------------------------------

export async function init(target, opts = {}) {
  const dir = path.resolve(target);
  const name = (opts.name || path.basename(dir)).replace(/[^A-Za-z0-9_-]/g, '-').replace(/^[^A-Za-z]+/, 'app-') || 'app';
  const template = path.join(cliRoot, 'templates', opts.template || 'app');
  if (!existsSync(template)) throw new BuildError(`unknown template '${opts.template || 'app'}'`);
  if (existsSync(dir) && (await readdir(dir)).length) throw new BuildError(`${dir} already exists and is not empty`);
  const typesDir = path.join(repoRoot, 'packages', 'types');
  const typesRel = existsSync(path.join(typesDir, 'index.d.ts')) ? path.relative(dir, typesDir).split(path.sep).join('/') : null;
  async function copyDir(src, dst) {
    await mkdir(dst, { recursive: true });
    for (const e of await readdir(src, { withFileTypes: true })) {
      const s = path.join(src, e.name), d = path.join(dst, e.name.replace(/\.tpl$/, ''));
      if (e.isDirectory()) await copyDir(s, d);
      else if (/\.(json|html|tsx?|css|md|tpl)$/.test(e.name)) {
        let text = await readFile(s, 'utf8');
        text = text.replaceAll('__NAME__', name).replaceAll('__TYPES__', typesRel || 'node_modules/rendrix');
        await writeFile(d, text);
      } else await copyFile(s, d);
    }
  }
  await copyDir(template, dir);
  ok(`created ${path.relative(process.cwd(), dir) || '.'} from the '${opts.template || 'app'}' template`);
  console.log(`\n  cd ${path.relative(process.cwd(), dir) || '.'}\n  rendrix dev      # build, run and rebuild on change\n  rendrix package  # produce a standalone executable\n`);
}

// ----- doctor ---------------------------------------------------------------------------------

export async function doctor(dir) {
  let bad = 0;
  const row = (good, label, detail = '') => { console.log(`${good ? color(32, '✓') : color(31, '✗')} ${label}${detail ? color(90, '  ' + detail) : ''}`); if (!good) bad++; };
  row(Number(process.versions.node.split('.')[0]) >= 18, 'Node.js', process.version);
  let tsv = '';
  try { tsv = (await import('typescript')).default.version; } catch { /* reported below */ }
  row(!!tsv, 'TypeScript compiler', tsv || 'not installed (npm i -D typescript)');
  const rt = findRuntime();
  row(!!rt, 'Rendrix runtime', rt || 'not found. Build with: cargo build --release -p rendrix-runtime --manifest-path engine/Cargo.toml (or set RENDRIX_RUNTIME)');
  row(true, 'release targets', `${currentTarget().key} (host); packaging: ${supportedTargets().join(', ')}`);
  if (rt) {
    const r = spawnSync(rt, ['doctor'], { encoding: 'utf8' });
    if (r.status === 0) for (const line of r.stdout.trim().split('\n')) row(!/no suitable|error|failed/i.test(line), line.split(':')[0].trim(), line.split(':').slice(1).join(':').trim());
    else row(false, 'runtime self-check', (r.stderr || r.stdout || 'failed to start').trim());
  }
  // S1: report the signing/installer toolchain so `package --sign --installer nsis` failures
  // are diagnosed here instead of at release time.
  if (process.platform === 'win32') {
    let makensis = false;
    try { makensis = spawnSync('makensis', ['-VERSION'], { encoding: 'utf8', windowsHide: true }).status === 0; } catch { /* absent */ }
    row(makensis, 'NSIS installer (makensis)', makensis ? 'available' : 'not found (install NSIS to build --installer nsis)');
    const tool = findSigntool();
    row(!!(tool || process.env.RENDRIX_SIGN_COMMAND), 'code signing', tool || process.env.RENDRIX_SIGN_COMMAND || 'unsigned (set RENDRIX_SIGN_COMMAND or RENDRIX_CERT_FILE to sign)');
  } else if (process.platform === 'darwin') {
    const tool = findCodesign();
    row(!!(tool || process.env.RENDRIX_SIGN_COMMAND), 'code signing', tool ? 'codesign available' : (process.env.RENDRIX_SIGN_COMMAND || 'unsigned (set RENDRIX_SIGN_COMMAND or RENDRIX_CODESIGN_IDENTITY to sign)'));
  } else {
    row(true, 'code signing', process.env.RENDRIX_SIGN_COMMAND || process.env.RENDRIX_GPG_KEY ? 'gpg signing configured' : 'unsigned (set RENDRIX_GPG_KEY to sign; Linux ships the raw binary)');
  }
  try {
    const p = await loadProject(dir);
    row(true, `project '${p.name}'`, `${p.width}x${p.height}, network ${p.permissions.network ? 'allowed' : 'denied'}`);
  } catch (e) { row(true, 'project', 'no rendrix.json here (fine outside a project)'); void e; }
  console.log(bad ? `\n${bad} problem${bad === 1 ? '' : 's'} found.` : '\nEverything looks good.');
  return bad ? 1 : 0;
}

// ----- render ---------------------------------------------------------------------------------
//
// `rendrix render page.html -o out.png|out.pdf|frames/` renders a single HTML page (or a
// directory holding one) headlessly: PNG, one-page PDF, or a PNG frame sequence that the
// /video pipeline consumes for mp4. The page is staged into a temp project (only the page
// plus the files it references) and built with typechecking off; the runtime does the
// pixels through the same headless path as `--screenshot`.

/** Classify `-o`: `.png` → screenshot, `.pdf` → PDF, anything else → frame directory. */
export function renderTarget(out) {
  const o = String(out || '');
  if (/\.png$/i.test(o)) return { kind: 'png' };
  if (/\.pdf$/i.test(o)) return { kind: 'pdf' };
  return { kind: 'frames' };
}

function parseRenderSize(size) {
  const m = /^(\d+)x(\d+)$/.exec(String(size || '1280x720'));
  const w = m ? Number(m[1]) : NaN, h = m ? Number(m[2]) : NaN;
  if (!Number.isInteger(w) || !Number.isInteger(h) || w < 100 || h < 100 || w > 8192 || h > 8192) {
    throw new BuildError(`render: --size must be WxH like 1280x720 (got '${size}')`);
  }
  return { w, h };
}

const RENDER_SKIP_DIRS = new Set(['node_modules', 'dist', '.git', '.rendrix', 'target']);
const RENDER_MAX_FILE = 8 * 1024 * 1024;

function isRemote(ref) {
  return /^[a-z][a-z0-9+.-]*:/i.test(ref) || ref.startsWith('data:') || ref.startsWith('#');
}

function cleanRef(ref) {
  return ref.split('#')[0].split('?')[0].trim();
}

/** Relative file references out of an HTML / CSS / JS source (first pass, regex-based). */
function scanRefs(kind, text) {
  const out = new Set();
  const push = r => {
    let c = cleanRef(r);
    if (!c || isRemote(c)) return;
    if (c.startsWith('/')) c = c.replace(/^\/+/, ''); // site-rooted: resolve against the page directory, like buildApp
    if (c) out.add(c);
  };
  if (kind === 'html') {
    for (const m of text.matchAll(/(?:src|href)\s*=\s*"([^"]+)"|(?:src|href)\s*=\s*'([^']+)'/gi)) push(m[1] ?? m[2]);
  } else if (kind === 'css') {
    for (const m of text.matchAll(/url\(\s*"([^"]+)"\s*\)|url\(\s*'([^']+)'\s*\)|url\(\s*([^"'()\s][^()\s]*)\s*\)/gi)) push(m[1] ?? m[2] ?? m[3]);
  } else {
    for (const m of text.matchAll(/(?:import\s+(?:[^'"]*?\s+from\s+)?|export\s+[^'"]*?\s+from\s+|import\()\s*["']([^"']+)["']/g)) push(m[1]);
  }
  return out;
}

/**
 * Stage a single page plus the files it references into `dst`.
 * `srcDir` is the page's directory (references resolve against it and must stay inside it).
 */
async function stagePage(srcDir, entryRel, dst, warnings) {
  const seen = new Set();
  async function stage(rel) {
    const norm = path.normalize(rel);
    if (seen.has(norm)) return;
    seen.add(norm);
    const abs = path.resolve(srcDir, norm);
    const under = path.relative(srcDir, abs);
    if (under.startsWith('..') || path.isAbsolute(under)) { warnings.push(`render: reference '${rel}' escapes the page directory and was skipped`); return; }
    let bytes;
    try {
      const info = await stat(abs);
      if (info.isDirectory()) return;
      if (info.size > RENDER_MAX_FILE) { warnings.push(`render: '${rel}' is larger than 8 MB and was skipped`); return; }
      bytes = await readFile(abs);
    } catch { warnings.push(`render: referenced file '${rel}' was not found and was skipped`); return; }
    await mkdir(path.dirname(path.join(dst, under)), { recursive: true });
    await writeFile(path.join(dst, under), bytes);
    const text = bytes.toString('utf8');
    const kind = /\.css$/i.test(under) ? 'css' : /\.(m?[jt]sx?)$/i.test(under) ? 'js' : null;
    if (kind && !text.includes('\uFFFD')) {
      for (const r of scanRefs(kind, text)) await stage(path.join(path.dirname(under), r).replace(/\\/g, '/'));
    }
  }
  const html = await readFile(path.resolve(srcDir, entryRel), 'utf8');
  for (const r of scanRefs('html', html)) await stage(r);
}

/** Copy a whole page directory minus tool output dirs and oversized files. */
async function stageDir(srcDir, dst, warnings) {
  async function walk(dir) {
    for (const e of await readdir(dir, { withFileTypes: true })) {
      if (e.name.startsWith('.') || RENDER_SKIP_DIRS.has(e.name)) continue;
      const full = path.join(dir, e.name);
      if (e.isDirectory()) { await walk(full); continue; }
      const rel = path.relative(srcDir, full);
      const info = await stat(full);
      if (info.size > RENDER_MAX_FILE) { warnings.push(`render: '${rel}' is larger than 8 MB and was skipped`); continue; }
      await mkdir(path.dirname(path.join(dst, rel)), { recursive: true });
      await copyFile(full, path.join(dst, rel));
    }
  }
  await walk(srcDir);
}

export async function render(input, opts = {}) {
  const { w, h } = parseRenderSize(opts.size);
  const scale = Number(opts.scale ?? 1);
  if (!(scale > 0) || scale > 4) throw new BuildError(`render: --scale must be between 0 and 4 (got '${opts.scale}')`);
  const target = renderTarget(opts.out || 'frames/');
  if (/\.mp4$/i.test(String(opts.out || ''))) {
    throw new BuildError('render: mp4 output belongs to the /video pipeline (`rendrix render` writes PNG, PDF or a PNG frame sequence for it to consume)');
  }
  const src = path.resolve(input);
  const st = existsSync(src) ? statSync(src) : null;
  if (!st) throw new BuildError(`render: '${input}' does not exist`);
  if (st.isFile() && !/\.html?$/i.test(src)) throw new BuildError(`render: '${input}' is not an HTML file (pass a .html page or a directory)`);
  const rt = needRuntime();
  const out = path.resolve(opts.out || 'frames/');
  const frames = Math.max(1, Math.min(3600, Number(opts.frames ?? 60) || 60));
  const frameMs = Math.max(0, Math.min(10_000, Number(opts.frameMs ?? opts.frame_ms ?? 50) || 0));

  const tmp = await mkdtemp(path.join(os.tmpdir(), 'rendrix-render-'));
  try {
    const warnings = [];
    let entry = 'index.html';
    if (st.isDirectory()) {
      await stageDir(src, tmp, warnings);
      if (!existsSync(path.join(tmp, 'index.html'))) {
        const htmls = (await readdir(tmp)).filter(f => /\.html?$/i.test(f)).sort();
        if (!htmls.length) throw new BuildError(`render: no .html file in '${input}'`);
        entry = htmls[0];
      }
    } else {
      if (!/\.html?$/i.test(src)) throw new BuildError(`render: '${input}' is not an HTML file (pass a .html page or a directory)`);
      const dir = path.dirname(src);
      await stagePage(dir, path.basename(src), tmp, warnings);
      await copyFile(src, path.join(tmp, 'index.html'));
    }
    await writeFile(path.join(tmp, 'rendrix.json'), JSON.stringify({ name: 'render', window: { width: w, height: h }, entry }));
    const { bundle, warnings: buildWarnings } = await buildApp(tmp, { typecheck: false });
    for (const warnText of [...warnings, ...buildWarnings]) warn(warnText);
    const bundlePath = path.join(tmp, 'page.rdxapp');
    await writeBundle(bundle, bundlePath);
    const args = ['run', bundlePath, '--headless', '--size', `${w}x${h}`, '--scale', String(scale)];
    if (opts.script) args.push('--script', path.resolve(opts.script));
    if (target.kind === 'png') args.push('--screenshot', out);
    else if (target.kind === 'pdf') args.push('--pdf', out);
    else {
      await mkdir(out, { recursive: true });
      args.push('--frame-dir', out, '--frames', String(frames), '--frame-ms', String(frameMs));
    }
    const r = spawnSync(rt, args, { stdio: 'inherit' });
    if (r.status !== 0) throw new BuildError(`render failed (runtime exit ${r.status ?? 'unknown'})`);
    if (target.kind === 'frames') ok(`rendered ${frames} frame${frames === 1 ? '' : 's'} (${w}x${h}@${scale}x) → ${path.relative(process.cwd(), out) || '.'}`);
    else ok(`rendered ${w}x${h}@${scale}x → ${path.relative(process.cwd(), out)}`);
    return { out, kind: target.kind };
  } finally {
    await rm(tmp, { recursive: true, force: true });
  }
}

export { BUNDLE_FORMAT };
