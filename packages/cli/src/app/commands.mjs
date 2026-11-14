// `rendrix` commands for application projects: init, check, build, run, dev, package, doctor.

import path from 'node:path';
import { spawn, spawnSync } from 'node:child_process';
import { existsSync, watch, statSync } from 'node:fs';
import { readFile, writeFile, mkdir, readdir, copyFile, chmod } from 'node:fs/promises';
import { fileURLToPath } from 'node:url';
import { buildApp, writeBundle, bundleStats, typeCheck, loadProject, BuildError, BUNDLE_FORMAT } from './build.mjs';
import { writeManifest, resolveSignCommand, signArtifact, notarizeArtifact, findSigntool, findCodesign } from '../signing.mjs';
import { nsisScript, buildNsis } from '../installer/nsis.mjs';

const here = path.dirname(fileURLToPath(import.meta.url));
const cliRoot = path.resolve(here, '..', '..');
const repoRoot = path.resolve(cliRoot, '..', '..');
export const MAGIC = Buffer.from('RENDRAPP');
const exe = process.platform === 'win32' ? '.exe' : '';

const color = (c, s) => (process.stdout.isTTY ? `\x1b[${c}m${s}\x1b[0m` : s);
const ok = s => console.log(color(32, '✓'), s);
const info = s => console.log(color(36, '›'), s);
const warn = s => console.warn(color(33, '!'), s);
const fail = s => console.error(color(31, '✗'), s);

/** Locate the prebuilt runtime executable. */
export function findRuntime() {
  const candidates = [
    process.env.RENDRIX_RUNTIME,
    path.join(repoRoot, 'engine', 'target', 'release', 'rendrix-runtime' + exe),
    path.join(repoRoot, 'engine', 'target', 'debug', 'rendrix-runtime' + exe),
    process.env.RENDRIX_HOME && path.join(process.env.RENDRIX_HOME, 'runtime', `${process.platform}-${process.arch}`, 'rendrix-runtime' + exe),
  ].filter(Boolean);
  return candidates.find(p => existsSync(p)) || null;
}

function needRuntime() {
  const rt = findRuntime();
  if (!rt) throw new BuildError('The Rendrix runtime is not built. Run `cargo build --release -p rendrix-runtime --manifest-path engine/Cargo.toml`, or set RENDRIX_RUNTIME to a rendrix-runtime executable. `rendrix doctor` shows what was searched.');
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

/** Append the bundle to a copy of the runtime executable (`[json][u64 LE length][RENDRAPP]`). */
export async function packExecutable(runtimePath, bundle, outFile) {
  let bytes = await readFile(runtimePath);
  if (bytes.subarray(bytes.length - 8).equals(MAGIC)) {
    const len = Number(bytes.readBigUInt64LE(bytes.length - 16));
    bytes = bytes.subarray(0, bytes.length - 16 - len);
  }
  const json = Buffer.from(JSON.stringify(bundle));
  const trailer = Buffer.alloc(16);
  trailer.writeBigUInt64LE(BigInt(json.length), 0);
  MAGIC.copy(trailer, 8);
  await mkdir(path.dirname(outFile), { recursive: true });
  await writeFile(outFile, Buffer.concat([bytes, json, trailer]));
  if (process.platform !== 'win32') await chmod(outFile, 0o755);
}

export async function pack(dir, opts = {}) {
  const { bundle, project } = await build(dir, { ...opts, out: opts.bundleOut });
  const rt = opts.runtime || needRuntime();
  const version = await appVersion(project.dir, opts);
  const artifacts = [];
  if (process.platform === 'darwin') {
    // macOS app bundle: dist/<Name>.app/Contents/{MacOS/<Name>,Info.plist}. The .app directory
    // is the distributable artifact (zip it; notarize the binary via signing.mjs helpers).
    const appDir = path.resolve(opts.out || path.join(project.dir, 'dist', `${project.name}.app`));
    const macos = path.join(appDir, 'Contents', 'MacOS');
    await mkdir(macos, { recursive: true });
    const exe = path.join(macos, project.name);
    await packExecutable(rt, bundle, exe);
    await writeFile(path.join(appDir, 'Contents', 'Info.plist'), macosPlist(project, version));
    artifacts.push(exe);
    ok(`packaged ${path.relative(process.cwd(), appDir)} (${(statSync(exe).size / 1048576).toFixed(1)} MB, standalone; needs no Node, Bun or browser)`);
    if (opts.installer && opts.installer !== 'none') throw new BuildError(`installer '${opts.installer}' is only supported on Windows (NSIS); on macOS zip the .app bundle`);
    return finishPack({ project, version, artifacts, appDir, out: exe }, opts);
  }
  const out = path.resolve(opts.out || path.join(project.dir, 'dist', project.name + exe));
  await packExecutable(rt, bundle, out);
  artifacts.push(out);
  ok(`packaged ${path.relative(process.cwd(), out)} (${(statSync(out).size / 1048576).toFixed(1)} MB, standalone; needs no Node, Bun or browser)`);
  let installer = null;
  if (opts.installer && opts.installer !== 'none') {
    if (opts.installer !== 'nsis') throw new BuildError(`unknown installer '${opts.installer}' (supported: none, nsis)`);
    if (process.platform !== 'win32') throw new BuildError('the NSIS installer can only be built on Windows');
    const scriptPath = path.join(path.dirname(out), `${project.name}.nsi`);
    const setupOut = path.join(path.dirname(out), `${project.name}-setup-${version}.exe`);
    info(`building NSIS installer → ${path.relative(process.cwd(), setupOut)}`);
    await buildNsis({ name: project.name, version, executable: out, output: setupOut, publishUrl: opts.publishUrl || '', certificateSubjectName: process.env.RENDRIX_CERT_SUBJECT || '' }, scriptPath);
    installer = setupOut;
    artifacts.push(installer);
  }
  return finishPack({ project, version, artifacts, out, installer }, opts);
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

function macosPlist(project, version) {
  const id = `dev.rendrix.${project.name.toLowerCase().replace(/[^a-z0-9]/g, '')}`;
  return `<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleName</key><string>${project.name}</string>
  <key>CFBundleDisplayName</key><string>${project.title}</string>
  <key>CFBundleIdentifier</key><string>${id}</string>
  <key>CFBundleVersion</key><string>${version}</string>
  <key>CFBundleShortVersionString</key><string>${version}</string>
  <key>CFBundleExecutable</key><string>${project.name}</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>LSMinimumSystemVersion</key><string>13.0</string>
  <key>NSHighResolutionCapable</key><true/>
</dict>
</plist>
`;
}

/// Sign artifacts, write the SHA-256 manifest (+ detached `.sig` with `--manifest-key`), and
/// notarize on macOS. Wiring for S1: `rendrix package --sign --installer nsis
/// --manifest dist/manifest.json --publish-url https://example.com/app`.
async function finishPack(pack, opts) {
  if (opts.sign) {
    const command = typeof opts.sign === 'string' ? opts.sign : resolveSignCommand();
    if (!command) throw new BuildError('signing requested but no signing command is configured (set RENDRIX_SIGN_COMMAND, or RENDRIX_CERT_FILE on Windows / RENDRIX_CODESIGN_IDENTITY on macOS)');
    for (const file of pack.artifacts) {
      info(`signing ${path.basename(file)}`);
      await signArtifact(file, command);
    }
    if (process.platform === 'darwin' && process.env.APPLE_ID) await notarizeArtifact(pack.out);
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

export { BUNDLE_FORMAT };
