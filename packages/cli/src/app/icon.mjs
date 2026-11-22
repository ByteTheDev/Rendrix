// Dependency-free icon material for the macOS .app and Linux desktop layouts. When a project
// has no icon we generate a small monochrome "R" tile, so every packaged app still gets a real
// .icns / PNG instead of a generic executable icon.

import path from 'node:path';
import { existsSync } from 'node:fs';
import { readFile } from 'node:fs/promises';
import { deflateRawSync, crc32 } from './archive.mjs';
import { BuildError } from './build.mjs';

const PNG_SIG = Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]);
const ICNS_PNG_TYPES = new Map([[16, 'icp4'], [32, 'icp5'], [64, 'icp6'], [128, 'ic07'], [256, 'ic08'], [512, 'ic09'], [1024, 'ic10']]);

function pngChunk(type, data) {
  const len = Buffer.alloc(4);
  len.writeUInt32BE(data.length, 0);
  const body = Buffer.concat([Buffer.from(type, 'ascii'), data]);
  const crc = Buffer.alloc(4);
  crc.writeUInt32BE(crc32(body), 0);
  return Buffer.concat([len, body, crc]);
}

/** Encode an RGBA byte buffer as a PNG (no external image libraries). */
export function encodePng(width, height, rgba) {
  const stride = width * 4;
  const raw = Buffer.alloc((stride + 1) * height);
  for (let y = 0; y < height; y++) {
    raw[y * (stride + 1)] = 0; // filter: none
    rgba.copy(raw, y * (stride + 1) + 1, y * stride, (y + 1) * stride);
  }
  const ihdr = Buffer.alloc(13);
  ihdr.writeUInt32BE(width, 0);
  ihdr.writeUInt32BE(height, 4);
  ihdr[8] = 8;  // bit depth
  ihdr[9] = 6;  // colour type: RGBA
  return Buffer.concat([
    PNG_SIG,
    pngChunk('IHDR', ihdr),
    pngChunk('IDAT', deflateRawSync(raw, { level: 9 })),
    pngChunk('IEND', Buffer.alloc(0)),
  ]);
}

const GLYPH_R = ['11110', '10001', '10001', '11110', '10100', '10010', '10001'];

function tilePixel(x, y) {
  const radius = 6;
  const cx = x < radius ? radius : (x > 31 - radius ? 31 - radius : x);
  const cy = y < radius ? radius : (y > 31 - radius ? 31 - radius : y);
  if ((x - cx) ** 2 + (y - cy) ** 2 > radius ** 2) return [0, 0, 0, 0];
  const lx = Math.floor((x - 8) / 3), ly = Math.floor((y - 5) / 3);
  if (lx >= 0 && lx < 5 && ly >= 0 && ly < 7 && GLYPH_R[ly][lx] === '1') return [245, 245, 245, 255];
  return [14, 14, 16, 255];
}

export function defaultIconPixels(size) {
  const out = Buffer.alloc(size * size * 4);
  for (let y = 0; y < size; y++) {
    for (let x = 0; x < size; x++) {
      const [r, g, b, a] = tilePixel(Math.floor(x * 32 / size), Math.floor(y * 32 / size));
      const i = (y * size + x) * 4;
      out[i] = r; out[i + 1] = g; out[i + 2] = b; out[i + 3] = a;
    }
  }
  return out;
}

export function defaultPng(size = 256) {
  return encodePng(size, size, defaultIconPixels(size));
}

function icnsChunk(type, data) {
  const header = Buffer.alloc(8);
  header.write(type, 0, 'ascii');
  header.writeUInt32BE(8 + data.length, 4);
  return Buffer.concat([header, data]);
}

/** A valid .icns containing PNG-encoded representations at the conventional sizes. */
export function defaultIcns(sizes = [16, 32, 64, 128, 256, 512]) {
  const chunks = sizes.map(size => {
    const type = ICNS_PNG_TYPES.get(size);
    if (!type) throw new Error(`unsupported icns icon size ${size}`);
    return icnsChunk(type, defaultPng(size));
  });
  const body = Buffer.concat(chunks);
  const header = Buffer.alloc(8);
  header.write('icns', 0, 'ascii');
  header.writeUInt32BE(8 + body.length, 4);
  return Buffer.concat([header, body]);
}

export function pngSize(bytes) {
  if (bytes.length < 24 || !bytes.subarray(0, 8).equals(PNG_SIG)) return null;
  return { width: bytes.readUInt32BE(16), height: bytes.readUInt32BE(20) };
}

/**
 * Find an icon passed via --icon or one of the conventional project filenames.
 * Returns `{ path, data, ext }`, or null when the generated default should be used.
 */
export async function resolveIcon(projectDir, iconOption) {
  let file = iconOption ? path.resolve(iconOption) : null;
  if (file && !existsSync(file)) throw new BuildError(`--icon '${iconOption}' does not exist`);
  if (!file) {
    for (const name of ['icon.icns', 'icon.png', 'app.icns', 'app.png']) {
      const candidate = path.join(projectDir, name);
      if (existsSync(candidate)) { file = candidate; break; }
    }
  }
  if (!file) return null;
  const ext = path.extname(file).toLowerCase();
  if (!['.png', '.icns'].includes(ext)) throw new BuildError(`--icon must be a .png or .icns file (got '${ext || file}')`);
  return { path: file, data: await readFile(file), ext };
}

/** Build an .icns from a user PNG, falling back to the generated default for non-square art. */
export function icnsFromIcon(icon, warn = () => {}) {
  if (!icon) return defaultIcns();
  if (icon.ext === '.icns') return icon.data;
  const size = pngSize(icon.data);
  const type = size && size.width === size.height ? ICNS_PNG_TYPES.get(size.width) : null;
  if (!type) {
    warn(`icon must be a square PNG at 16/32/64/128/256/512/1024 px for .icns output; using the generated icon instead`);
    return defaultIcns();
  }
  const header = Buffer.alloc(8);
  header.write('icns', 0, 'ascii');
  header.writeUInt32BE(16 + icon.data.length, 4);
  return Buffer.concat([header, icnsChunk(type, icon.data)]);
}

/** PNG bytes for the Linux hicolor icon (user art when square, otherwise generated). */
export function pngFromIcon(icon, warn = () => {}) {
  if (!icon) return defaultPng(256);
  if (icon.ext === '.png') {
    const size = pngSize(icon.data);
    if (size && size.width === size.height) return icon.data;
    warn('icon must be a square PNG for the Linux desktop entry; using the generated icon instead');
  } else {
    warn('.icns icons cannot be used for the Linux desktop entry; using the generated icon instead');
  }
  return defaultPng(256);
}
