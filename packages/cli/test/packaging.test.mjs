import test from 'node:test';
import assert from 'node:assert/strict';
import path from 'node:path';
import os from 'node:os';
import { mkdtemp, mkdir, writeFile, readFile, rm } from 'node:fs/promises';
import { resolveTarget, isCrossTarget, currentTarget, supportedTargets } from '../src/app/target.mjs';
import { crc32, zipStore, tarGz, tarHeader } from '../src/app/archive.mjs';
import { xmlEscape, macosPlist, desktopFile, bundleId, packExecutable, writeMacApp, writeLinuxRoot, MAGIC } from '../src/app/packaging.mjs';
import { encodePng, defaultPng, defaultIcns, pngSize, icnsFromIcon, pngFromIcon } from '../src/app/icon.mjs';
import { splitCommand, signtoolRoots, signtoolCandidates } from '../src/signing.mjs';

const cleanup = dir => rm(dir, { recursive: true, force: true });

// Temporarily override host platform/arch so host-dependent logic can be exercised in isolation.
// Both properties are configurable in Node; the original descriptors are always restored.
function withHost(platform, arch, fn) {
  const platformDesc = Object.getOwnPropertyDescriptor(process, 'platform');
  const archDesc = Object.getOwnPropertyDescriptor(process, 'arch');
  const define = (name, value) => Object.defineProperty(process, name, {
    value, configurable: true, enumerable: true, writable: false,
  });
  try {
    define('platform', platform);
    define('arch', arch);
    return fn();
  } finally {
    Object.defineProperty(process, 'platform', platformDesc);
    Object.defineProperty(process, 'arch', archDesc);
  }
}

test('resolveTarget accepts distribution keys, Rust triples and short aliases', () => {
  assert.deepEqual(resolveTarget('darwin-arm64'), { platform: 'darwin', arch: 'arm64', key: 'darwin-arm64', exe: '' });
  assert.deepEqual(resolveTarget('aarch64-apple-darwin'), resolveTarget('macos'));
  assert.deepEqual(resolveTarget('x86_64-pc-windows-msvc'), { platform: 'win32', arch: 'x64', key: 'win32-x64', exe: '.exe' });
  assert.deepEqual(resolveTarget('X86_64-UNKNOWN-LINUX-GNU'), resolveTarget('linux-x64'));
  assert.deepEqual(resolveTarget(undefined), currentTarget());
  assert.throws(() => resolveTarget('plan9-arm64'), /unknown target.*plan9-arm64/);
  assert.equal(supportedTargets().length, 7);
  assert.ok(supportedTargets().includes('win32-x86'));
});

test('resolveTarget recognizes 32-bit x86 Windows names without confusing x86_64', () => {
  const expected = { platform: 'win32', arch: 'x86', key: 'win32-x86', exe: '.exe' };
  for (const alias of [
    'win32-x86', 'win32-ia32', 'ia32', 'x86', 'i386', 'i486', 'i586', 'i686',
    'i386-pc-windows-msvc', 'i686-pc-windows-msvc', 'i686-pc-windows-gnu',
    'i686-windows', 'x86-pc-windows-msvc', 'windows-x86', 'windows-ia32', 'win-x86',
  ]) {
    assert.deepEqual(resolveTarget(alias), expected, `alias '${alias}'`);
  }
  assert.equal(resolveTarget('x86_64-pc-windows-msvc').key, 'win32-x64', 'x86_64 stays 64-bit');
  assert.equal(resolveTarget('win32-x86_64').key, 'win32-x64', 'win32-x86_64 stays 64-bit');
});

test('cross-target detection compares target to the host', () => {
  const host = currentTarget();
  assert.equal(isCrossTarget(resolveTarget(host.key)), false);
  const other = supportedTargets().find(k => k !== host.key);
  assert.equal(isCrossTarget(resolveTarget(other)), true);
});

test('currentTarget maps a 32-bit x86 Windows host to win32-x86', () => {
  const expected = { platform: 'win32', arch: 'x86', key: 'win32-x86', exe: '.exe' };
  withHost('win32', 'ia32', () => assert.deepEqual(currentTarget(), expected));
  withHost('win32', 'x86', () => assert.deepEqual(currentTarget(), expected));
  // Non-Windows hosts keep the existing fallback behavior unchanged.
  withHost('linux', 'ia32', () => {
    assert.deepEqual(currentTarget(), { platform: 'linux', arch: 'ia32', key: 'linux-ia32', exe: '' });
  });
});

test('isCrossTarget treats win32-x86 as distinct from other Windows targets', () => {
  for (const hostArch of ['ia32', 'x86']) {
    withHost('win32', hostArch, () => {
      assert.equal(isCrossTarget(resolveTarget('win32-x86')), false, `host arch ${hostArch}`);
      assert.equal(isCrossTarget(resolveTarget('win32-x64')), true, `host arch ${hostArch}`);
      assert.equal(isCrossTarget(resolveTarget('win32-arm64')), true, `host arch ${hostArch}`);
    });
  }
});

test('splitCommand tokenizes quotes without a shell', () => {
  assert.deepEqual(splitCommand('codesign --sign "My Identity" --deep'), ['codesign', '--sign', 'My Identity', '--deep']);
  assert.deepEqual(splitCommand('tool /f "C:\\cert.pfx" /p "a b"'), ['tool', '/f', 'C:\\cert.pfx', '/p', 'a b']);
  assert.deepEqual(splitCommand('a  b\tc'), ['a', 'b', 'c']);
  assert.throws(() => splitCommand('a "unterminated'), /unterminated quote/);
});

test('signtool discovery uses environment roots and prefers the host architecture', () => {
  const roots = ['/sdk/10', '/sdk/8.1'];
  const candidates = signtoolCandidates(roots, 'x64');
  assert.equal(candidates[0], path.join('/sdk/10', 'x64', 'signtool.exe'));
  assert.ok(candidates.some(p => p.includes('arm64')));
  assert.ok(signtoolRoots().some(p => p.includes('Windows Kits')));
  assert.ok(signtoolRoots().every(p => !p.includes('C:\\Program Files (x86)\\Windows Kits\\10\\bin\\')));
});

test('plist and desktop entry escape values and declare the icon', () => {
  const project = { name: 'my-app', title: 'A & B <Tools>' };
  assert.equal(xmlEscape('a&b<c>"d"'), 'a&amp;b&lt;c&gt;&quot;d&quot;');
  assert.equal(bundleId(project), 'dev.rendrix.myapp');
  const plist = macosPlist(project, '1.2.3', { executable: 'my-app', iconName: 'my-app.icns' });
  assert.match(plist, /A &amp; B &lt;Tools&gt;/);
  assert.match(plist, /<key>CFBundleIconFile<\/key><string>my-app\.icns<\/string>/);
  assert.match(plist, /<key>CFBundleExecutable<\/key><string>my-app<\/string>/);
  const desktop = desktopFile(project, { executable: 'my-app', iconName: 'my-app' });
  assert.match(desktop, /Name=A & B <Tools>/);
  assert.match(desktop, /Exec=my-app\n/);
  assert.match(desktop, /Icon=my-app\n/);
});

test('crc32, zip and tar writers produce the expected framing', () => {
  assert.equal(crc32(Buffer.from('123456789')), 0xcbf43926);
  const zip = zipStore([{ name: 'a/b.txt', data: 'hi', mode: 0o755 }]);
  assert.equal(zip.readUInt32LE(0), 0x04034b50, 'local file header');
  assert.equal(zip.subarray(30, 32).toString(), 'a/', 'UTF-8 POSIX name');
  assert.ok(zip.subarray(zip.length - 22, zip.length - 18).equals(Buffer.from([0x50, 0x4b, 0x05, 0x06])), 'EOCD');
  const h = tarHeader('bin/app', 5, 0o755);
  assert.equal(h.subarray(257, 265).toString(), 'ustar\0' + '00');
  assert.equal(h[156], 0x30, 'regular file typeflag');
  const tar = tarGz([{ name: 'bin/app', data: 'hello' }]);
  assert.equal(tar[0], 0x1f, 'gzip magic');
  assert.equal(tar[1], 0x8b);
});

test('generated icons are valid PNG/ICNS and user art is validated', () => {
  assert.deepEqual(pngSize(defaultPng(64)), { width: 64, height: 64 });
  const icns = defaultIcns();
  assert.equal(icns.subarray(0, 4).toString(), 'icns');
  assert.equal(icns.readUInt32BE(4), icns.length);
  assert.match(icns.subarray(8, 16).toString('latin1'), /icp4/);
  const square = encodePng(32, 32, Buffer.alloc(32 * 32 * 4, 255));
  assert.deepEqual(pngSize(square), { width: 32, height: 32 });
  assert.equal(icnsFromIcon({ ext: '.png', data: square }).subarray(8, 12).toString(), 'icp5');
  const warnings = [];
  const wide = encodePng(64, 32, Buffer.alloc(64 * 32 * 4, 255));
  icnsFromIcon({ ext: '.png', data: wide }, m => warnings.push(m));
  assert.equal(warnings.length, 1, 'non-square art falls back with a warning');
  assert.equal(pngFromIcon({ ext: '.icns', data: icns }, m => warnings.push(m)).subarray(1, 4).toString(), 'PNG');
});

test('packExecutable writes a re-packable trailer for a target', async () => {
  const dir = await mkdtemp(path.join(os.tmpdir(), 'rendrix-pack-test-'));
  try {
    const runtime = path.join(dir, 'rendrix-runtime');
    await writeFile(runtime, Buffer.from('fake-runtime'));
    const out = path.join(dir, 'app');
    await packExecutable(runtime, { format: 'rendrix-app/1', name: 'one' }, out, resolveTarget('linux-x64'));
    let bytes = await readFile(out);
    assert.ok(bytes.subarray(bytes.length - 8).equals(MAGIC));
    assert.equal(bytes.readBigUInt64LE(bytes.length - 16), BigInt(JSON.stringify({ format: 'rendrix-app/1', name: 'one' }).length));
    await packExecutable(out, { format: 'rendrix-app/1', name: 'two' }, out, resolveTarget('linux-x64'));
    bytes = await readFile(out);
    assert.ok(bytes.subarray(0, 12).equals(Buffer.from('fake-runtime')), 'old trailer removed');
    assert.ok(bytes.toString('utf8').includes('"two"'));
  } finally { await cleanup(dir); }
});

test('packExecutable writes a re-packable trailer for win32-x86', async () => {
  const dir = await mkdtemp(path.join(os.tmpdir(), 'rendrix-pack-x86-'));
  try {
    const runtime = path.join(dir, 'rendrix-runtime.exe');
    await writeFile(runtime, Buffer.from('fake-x86-runtime'));
    const out = path.join(dir, 'app.exe');
    const bundle = { format: 'rendrix-app/1', name: 'x86-app' };
    await packExecutable(runtime, bundle, out, resolveTarget('win32-x86'));
    let bytes = await readFile(out);
    assert.ok(bytes.subarray(bytes.length - 8).equals(MAGIC));
    assert.equal(bytes.readBigUInt64LE(bytes.length - 16), BigInt(JSON.stringify(bundle).length));
    // Re-pack via a Rust-triple alias to prove the trailer is removable and stays win32.
    await packExecutable(out, { format: 'rendrix-app/1', name: 'x86-app-2' }, out, resolveTarget('i686-pc-windows-msvc'));
    bytes = await readFile(out);
    assert.ok(bytes.subarray(0, 16).equals(Buffer.from('fake-x86-runtime')), 'old trailer removed');
    assert.ok(bytes.toString('utf8').includes('"x86-app-2"'));
  } finally { await cleanup(dir); }
});

test('macOS and Linux layouts are assembled with metadata and icons', async () => {
  const dir = await mkdtemp(path.join(os.tmpdir(), 'rendrix-layout-test-'));
  try {
    const runtime = path.join(dir, 'rendrix-runtime');
    await writeFile(runtime, Buffer.from('fake-runtime'));
    const project = { name: 'demo', title: 'Demo', dir };
    const bundle = { format: 'rendrix-app/1', modules: {} };
    const target = resolveTarget('darwin-arm64');

    const appDir = path.join(dir, 'demo.app');
    await writeMacApp({ appDir, project, version: '1.0.0', runtime, bundle, target, icon: null, icns: defaultIcns() });
    assert.ok(await readFile(path.join(appDir, 'Contents', 'Info.plist'), 'utf8').then(s => s.includes('CFBundleIconFile')));
    assert.equal((await readFile(path.join(appDir, 'Contents', 'PkgInfo'), 'utf8')).trim(), 'APPLRNDX');
    assert.equal((await readFile(path.join(appDir, 'Contents', 'Resources', 'demo.icns'))).subarray(0, 4).toString(), 'icns');
    assert.ok((await readFile(path.join(appDir, 'Contents', 'MacOS', 'demo'))).subarray(-8).equals(MAGIC));
    const zip = await import('../src/app/packaging.mjs').then(m => m.zipDirectory(appDir));
    assert.equal(zip.readUInt32LE(0), 0x04034b50);

    const root = path.join(dir, 'linux-root');
    await writeLinuxRoot({ root, project, version: '1.0.0', runtime, bundle, target: resolveTarget('linux-x64'), png: defaultPng(256) });
    assert.ok((await readFile(path.join(root, 'bin', 'demo'))).subarray(-8).equals(MAGIC));
    const desktop = await readFile(path.join(root, 'share', 'applications', 'dev.rendrix.demo.desktop'), 'utf8');
    assert.match(desktop, /\[Desktop Entry\]/);
    assert.match(desktop, /Exec=demo/);
    assert.equal((await readFile(path.join(root, 'share', 'icons', 'hicolor', '256x256', 'apps', 'demo.png'))).subarray(1, 4).toString(), 'PNG');
    const tar = await import('../src/app/packaging.mjs').then(m => m.tarDirectory(root));
    assert.equal(tar[0], 0x1f);
  } finally { await cleanup(dir); }
});
