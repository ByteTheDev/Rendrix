import test from 'node:test';
import assert from 'node:assert/strict';
import path from 'node:path';
import { signtoolCandidates, normalizeSignArch } from '../src/signing.mjs';
import { nsisScript } from '../src/installer/nsis.mjs';

const SDK_ROOT = path.join('sdk', '10');
const first = arch => signtoolCandidates([SDK_ROOT], arch)[0];

test('normalizeSignArch folds every 32-bit x86 name onto x86', () => {
  for (const arch of ['ia32', 'x86', 'i386', 'i486', 'i586', 'i686', 'IA32', ' x86 ']) {
    assert.equal(normalizeSignArch(arch), 'x86', arch);
  }
  assert.equal(normalizeSignArch('arm64'), 'arm64');
  assert.equal(normalizeSignArch('aarch64'), 'arm64');
  assert.equal(normalizeSignArch('x64'), 'x64');
  assert.equal(normalizeSignArch('x86_64'), 'x64');
  assert.equal(normalizeSignArch('nonsense'), 'x64');
});

test('signtoolCandidates prefers the x86 folder for every 32-bit x86 alias', () => {
  for (const arch of ['ia32', 'x86', 'i686', 'i386', 'i586']) {
    assert.equal(first(arch), path.join(SDK_ROOT, 'x86', 'signtool.exe'), arch);
  }
  // The full fallback order must stay x86 -> x64 -> arm64 for 32-bit targets.
  assert.deepEqual(
    signtoolCandidates([SDK_ROOT], 'x86'),
    ['x86', 'x64', 'arm64'].map(a => path.join(SDK_ROOT, a, 'signtool.exe'))
  );
});

test('signtoolCandidates keeps the x64 and arm64 preference orders', () => {
  assert.deepEqual(
    signtoolCandidates([SDK_ROOT], 'x64'),
    ['x64', 'x86', 'arm64'].map(a => path.join(SDK_ROOT, a, 'signtool.exe'))
  );
  assert.deepEqual(
    signtoolCandidates([SDK_ROOT], 'arm64'),
    ['arm64', 'x64', 'x86'].map(a => path.join(SDK_ROOT, a, 'signtool.exe'))
  );
});

const base = { name: 'MyApp', version: '1.0.0', executable: '/tmp/MyApp.exe', output: '/tmp/MyApp-setup.exe' };

test('nsisScript selects the 32-bit install dir for x86', () => {
  const script = nsisScript({ ...base, arch: 'x86' });
  assert.ok(script.includes('InstallDir "$PROGRAMFILES32\\MyApp"'));
  assert.ok(script.includes('; Target architecture: x86'));
  assert.ok(!script.includes('InstallDir "$PROGRAMFILES64'));
});

test('nsisScript selects the 64-bit install dir for x64 and arm64', () => {
  for (const arch of ['x64', 'arm64']) {
    const script = nsisScript({ ...base, arch });
    assert.ok(script.includes('InstallDir "$PROGRAMFILES64\\MyApp"'), arch);
    assert.ok(script.includes(`; Target architecture: ${arch}`));
  }
});

test('nsisScript defaults safely when arch is omitted or unknown', () => {
  for (const arch of [undefined, 'bogus']) {
    const script = nsisScript(arch === undefined ? { ...base } : { ...base, arch });
    assert.ok(script.includes('InstallDir "$PROGRAMFILES\\MyApp"'));
    assert.ok(!script.includes('$PROGRAMFILES32'));
    assert.ok(!script.includes('$PROGRAMFILES64'));
  }
});
