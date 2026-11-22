// Assemble distributable app layouts for each target. Kept free of CLI parsing so the layout
// builders are unit-testable and the same code runs when cross-packaging from any host.

import path from 'node:path';
import { readdir, readFile, writeFile, mkdir, chmod, copyFile, stat } from 'node:fs/promises';
import { zipStore, tarGz } from './archive.mjs';
import { pngSize } from './icon.mjs';

export const MAGIC = Buffer.from('RENDRAPP');

/** Append the bundle to a copy of the runtime (`[json][u64 LE length][RENDRAPP]`). */
export async function packExecutable(runtimePath, bundle, outFile, target) {
  const win = target ? target.platform === 'win32' : process.platform === 'win32';
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
  if (!win) await chmod(outFile, 0o755);
}

export function xmlEscape(value) {
  return String(value ?? '').replace(/[&<>"']/g, c => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&apos;' })[c]);
}

export function bundleId(project) {
  return `dev.rendrix.${String(project.name).toLowerCase().replace(/[^a-z0-9]+/g, '') || 'app'}`;
}

/** Info.plist for a complete .app bundle (icon and executable are declared when present). */
export function macosPlist(project, version, { executable, iconName } = {}) {
  const lines = [
    ['CFBundleName', project.name],
    ['CFBundleDisplayName', project.title || project.name],
    ['CFBundleIdentifier', bundleId(project)],
    ['CFBundleVersion', version],
    ['CFBundleShortVersionString', version],
    ['CFBundleExecutable', executable || project.name],
    ['CFBundlePackageType', 'APPL'],
    ['CFBundleSignature', 'RNDX'],
    ['LSMinimumSystemVersion', '13.0'],
    ['NSHighResolutionCapable', true],
  ];
  if (iconName) lines.push(['CFBundleIconFile', iconName]);
  const entries = lines.map(([key, value]) =>
    `  <key>${xmlEscape(key)}</key>${value === true ? '<true/>' : `<string>${xmlEscape(value)}</string>`}`).join('\n');
  return `<?xml version="1.0" encoding="UTF-8"?>\n<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">\n<plist version="1.0">\n<dict>\n${entries}\n</dict>\n</plist>\n`;
}

/** Create a fully formed .app bundle and return its executable and icon paths. */
export async function writeMacApp({ appDir, project, version, runtime, bundle, target, icon, icns }) {
  const contents = path.join(appDir, 'Contents');
  const macos = path.join(contents, 'MacOS');
  const resources = path.join(contents, 'Resources');
  await mkdir(macos, { recursive: true });
  await mkdir(resources, { recursive: true });
  const executable = path.join(macos, project.name);
  await packExecutable(runtime, bundle, executable, target);
  const iconName = `${project.name}.icns`;
  await writeFile(path.join(resources, iconName), icns);
  await writeFile(path.join(contents, 'Info.plist'), macosPlist(project, version, { executable: project.name, iconName }));
  await writeFile(path.join(contents, 'PkgInfo'), 'APPLRNDX');
  if (icon && icon.ext === '.icns') await copyFile(icon.path, path.join(resources, iconName));
  return { executable, iconPath: path.join(resources, iconName), appDir };
}

/** Recursively collect files below `dir` as ZIP entries with POSIX-relative names. */
export async function zipDirectory(dir, prefix = '') {
  const entries = [];
  async function walk(current) {
    for (const e of await readdir(current, { withFileTypes: true })) {
      const full = path.join(current, e.name);
      if (e.isDirectory()) { await walk(full); continue; }
      const st = await stat(full);
      const relative = path.relative(dir, full).split(path.sep).join('/');
      entries.push({
        name: prefix ? `${prefix}/${relative}` : relative,
        data: await readFile(full),
        mode: (st.mode & 0o777) || 0o644,
      });
    }
  }
  await walk(dir);
  return zipStore(entries);
}

export function desktopFile(project, { executable, iconName } = {}) {
  const name = project.title || project.name;
  return [
    '[Desktop Entry]',
    'Type=Application',
    'Version=1.0',
    `Name=${name}`,
    `Exec=${executable || project.name}`,
    `Icon=${iconName || project.name}`,
    'Terminal=false',
    'Categories=Utility;',
    '',
  ].join('\n');
}

/** Create the Linux staging tree (bin + freedesktop desktop entry + hicolor icon). */
export async function writeLinuxRoot({ root, project, version, runtime, bundle, target, png }) {
  const binDir = path.join(root, 'bin');
  const appDir = path.join(root, 'share', 'applications');
  const iconSize = pngSize(png)?.width || 256;
  const iconDir = path.join(root, 'share', 'icons', 'hicolor', `${iconSize}x${iconSize}`, 'apps');
  await mkdir(binDir, { recursive: true });
  await mkdir(appDir, { recursive: true });
  await mkdir(iconDir, { recursive: true });
  const executable = path.join(binDir, project.name);
  await packExecutable(runtime, bundle, executable, target);
  const desktopName = `${bundleId(project)}.desktop`;
  await writeFile(path.join(appDir, desktopName), desktopFile(project, { executable: project.name, iconName: project.name }));
  await writeFile(path.join(iconDir, `${project.name}.png`), png);
  void version;
  return { executable, root };
}

/** gzip-compressed tar of a staging tree, with paths relative to its root. */
export async function tarDirectory(root) {
  const entries = [];
  async function walk(current) {
    for (const e of await readdir(current, { withFileTypes: true })) {
      const full = path.join(current, e.name);
      if (e.isDirectory()) { await walk(full); continue; }
      const st = await stat(full);
      entries.push({
        name: path.relative(root, full).split(path.sep).join('/'),
        data: await readFile(full),
        mode: (st.mode & 0o777) || 0o644,
      });
    }
  }
  await walk(root);
  return tarGz(entries);
}
