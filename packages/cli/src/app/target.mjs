// Release target resolution. A target is the pair the runtime distribution is keyed by
// (`<platform>-<arch>`, e.g. `win32-x64`), plus the conventional executable suffix.
// All packaging decisions (runtime lookup, bundle layout, installer, icon format) use the
// *target*, never the host, so `rendrix package --target ...` can cross-package safely.

import { BuildError } from './build.mjs';

const TARGETS = new Map([
  ['win32-x64', { platform: 'win32', arch: 'x64' }],
  ['win32-x86', { platform: 'win32', arch: 'x86' }],
  ['win32-arm64', { platform: 'win32', arch: 'arm64' }],
  ['darwin-x64', { platform: 'darwin', arch: 'x64' }],
  ['darwin-arm64', { platform: 'darwin', arch: 'arm64' }],
  ['linux-x64', { platform: 'linux', arch: 'x64' }],
  ['linux-arm64', { platform: 'linux', arch: 'arm64' }],
]);

// Rust triples and short aliases → the distribution key above. `macos` is accepted as an
// alias for `darwin`; platform-only aliases pick a conventional default arch (x64 on
// Windows/Linux, arm64 on macOS). Use an explicit triple or `<platform>-<arch>` key to be exact.
const ALIASES = new Map([
  ['windows', 'win32-x64'], ['win', 'win32-x64'], ['win32', 'win32-x64'],
  ['x86_64-pc-windows-msvc', 'win32-x64'], ['x86_64-pc-windows-gnu', 'win32-x64'],
  ['x86_64-windows', 'win32-x64'], ['win32-x86_64', 'win32-x64'],
  // 32-bit x86 Windows. `x86_64`/`x86-64` are 64-bit and intentionally map to win32-x64 above.
  ['win32-x86', 'win32-x86'], ['win32-ia32', 'win32-x86'],
  ['x86', 'win32-x86'], ['ia32', 'win32-x86'],
  ['i386', 'win32-x86'], ['i486', 'win32-x86'], ['i586', 'win32-x86'], ['i686', 'win32-x86'],
  ['i386-pc-windows-msvc', 'win32-x86'], ['i686-pc-windows-msvc', 'win32-x86'],
  ['i686-pc-windows-gnu', 'win32-x86'], ['i686-windows', 'win32-x86'],
  ['x86-pc-windows-msvc', 'win32-x86'], ['windows-x86', 'win32-x86'],
  ['windows-ia32', 'win32-x86'], ['win-x86', 'win32-x86'],
  ['aarch64-pc-windows-msvc', 'win32-arm64'], ['arm64-pc-windows-msvc', 'win32-arm64'],
  ['aarch64-windows', 'win32-arm64'], ['win32-aarch64', 'win32-arm64'],
  ['macos', 'darwin-arm64'], ['darwin', 'darwin-arm64'], ['mac', 'darwin-arm64'],
  ['apple-darwin', 'darwin-arm64'], ['aarch64-apple-darwin', 'darwin-arm64'],
  ['arm64-apple-darwin', 'darwin-arm64'], ['macos-arm64', 'darwin-arm64'],
  ['darwin-aarch64', 'darwin-arm64'],
  ['x86_64-apple-darwin', 'darwin-x64'], ['macos-x64', 'darwin-x64'],
  ['macos-intel', 'darwin-x64'], ['darwin-x86_64', 'darwin-x64'],
  ['linux', 'linux-x64'], ['x86_64-unknown-linux-gnu', 'linux-x64'],
  ['x86_64-unknown-linux-musl', 'linux-x64'], ['x86_64-linux', 'linux-x64'],
  ['aarch64-unknown-linux-gnu', 'linux-arm64'], ['aarch64-unknown-linux-musl', 'linux-arm64'],
  ['arm64-unknown-linux-gnu', 'linux-arm64'], ['aarch64-linux', 'linux-arm64'],
]);

export function currentTarget() {
  // Node reports 32-bit x86 as `ia32`; the distribution key/arch field uses `x86`.
  const arch = process.platform === 'win32' && (process.arch === 'ia32' || process.arch === 'x86')
    ? 'x86'
    : process.arch;
  return fromKey(`${process.platform}-${arch}`) || {
    platform: process.platform, arch: process.arch,
    key: `${process.platform}-${process.arch}`, exe: process.platform === 'win32' ? '.exe' : '',
  };
}

function fromKey(key) {
  const entry = TARGETS.get(key);
  if (!entry) return null;
  return { ...entry, key, exe: entry.platform === 'win32' ? '.exe' : '' };
}

/** Resolve a Rust triple, short alias or `<platform>-<arch>` key into a release target. */
export function resolveTarget(value) {
  if (!value) return currentTarget();
  const raw = String(value).trim().toLowerCase();
  const key = ALIASES.get(raw) || raw;
  const target = fromKey(key);
  if (!target) {
    throw new BuildError(`unknown target '${value}' (supported: ${[...TARGETS.keys()].join(', ')} or the equivalent Rust triple)`);
  }
  return target;
}

/** True when the packaged runtime must come from a distribution, not a local host build. */
export function isCrossTarget(target) {
  const host = currentTarget();
  return target.platform !== host.platform || target.arch !== host.arch;
}

export function supportedTargets() {
  return [...TARGETS.keys()];
}
