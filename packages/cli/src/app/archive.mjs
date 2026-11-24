// Small, dependency-free archive writers used by `rendrix package`. Both formats are needed
// for cross-target distribution: a .app must be zipped for notarization and a Linux layout
// is shipped as a tarball. Using an in-process writer keeps cross-packaging working on a host
// that has no `zip`/`tar` binary (notably Windows).

import { deflateRawSync, gzipSync } from 'node:zlib';

const CRC_TABLE = (() => {
  const table = new Uint32Array(256);
  for (let n = 0; n < 256; n++) {
    let c = n;
    for (let k = 0; k < 8; k++) c = (c & 1) ? (0xedb88320 ^ (c >>> 1)) : (c >>> 1);
    table[n] = c >>> 0;
  }
  return table;
})();

export function crc32(data) {
  let c = 0xffffffff;
  for (let i = 0; i < data.length; i++) c = CRC_TABLE[(c ^ data[i]) & 0xff] ^ (c >>> 8);
  return (c ^ 0xffffffff) >>> 0;
}

function field(size, value) {
  const buf = Buffer.alloc(size);
  buf.write(String(value), 0, 'ascii');
  return buf;
}

/** Store-only ZIP (no deflate, valid on macOS/Linux/Windows). Entries use POSIX relative names. */
export function zipStore(entries) {
  const locals = [];
  const centrals = [];
  let offset = 0;
  for (const entry of entries) {
    const name = Buffer.from(entry.name.replace(/\\/g, '/'), 'utf8');
    const data = Buffer.isBuffer(entry.data) ? entry.data : Buffer.from(String(entry.data));
    const crc = crc32(data);
    const mode = (entry.mode ?? 0o644) & 0xffff;
    const local = Buffer.alloc(30);
    local.writeUInt32LE(0x04034b50, 0);
    local.writeUInt16LE(20, 4);           // version needed
    local.writeUInt16LE(0x0800, 6);       // UTF-8 names
    local.writeUInt16LE(0, 8);            // stored
    local.writeUInt16LE(0, 10);           // time
    local.writeUInt16LE(0x0021, 12);      // date: 1980-01-01 (deterministic)
    local.writeUInt32LE(crc, 14);
    local.writeUInt32LE(data.length, 18);
    local.writeUInt32LE(data.length, 22);
    local.writeUInt16LE(name.length, 26);
    local.writeUInt16LE(0, 28);
    const localBlock = Buffer.concat([local, name, data]);
    locals.push(localBlock);

    const central = Buffer.alloc(46);
    central.writeUInt32LE(0x02014b50, 0);
    central.writeUInt16LE(0x0314, 4);     // made by Unix, spec 2.0
    central.writeUInt16LE(20, 6);
    central.writeUInt16LE(0x0800, 8);
    central.writeUInt16LE(0, 10);
    central.writeUInt16LE(0, 12);
    central.writeUInt16LE(0x0021, 14);
    central.writeUInt32LE(crc, 16);
    central.writeUInt32LE(data.length, 20);
    central.writeUInt32LE(data.length, 24);
    central.writeUInt16LE(name.length, 28);
    central.writeUInt16LE(0, 30);         // extra
    central.writeUInt16LE(0, 32);         // comment
    central.writeUInt16LE(0, 34);         // disk
    central.writeUInt16LE(0, 36);         // internal attrs
    central.writeUInt32LE(mode << 16, 38);
    central.writeUInt32LE(offset, 42);
    centrals.push(Buffer.concat([central, name]));
    offset += localBlock.length;
  }
  const centralSize = centrals.reduce((n, b) => n + b.length, 0);
  const end = Buffer.alloc(22);
  end.writeUInt32LE(0x06054b50, 0);
  end.writeUInt16LE(entries.length, 8);
  end.writeUInt16LE(entries.length, 10);
  end.writeUInt32LE(centralSize, 12);
  end.writeUInt32LE(offset, 16);
  return Buffer.concat([...locals, ...centrals, end]);
}

function octal(size, value) {
  return field(size, value.toString(8).padStart(size - 1, '0') + '\0');
}

/** ustar header for one entry. Long names are split into prefix + name. */
export function tarHeader(name, size, mode = 0o644, type = '0') {
  const h = Buffer.alloc(512);
  let prefix = '';
  let n = name.replace(/\\/g, '/');
  if (Buffer.byteLength(n) > 100) {
    const slash = n.lastIndexOf('/', n.length - 101);
    if (slash > 0) { prefix = n.slice(0, slash); n = n.slice(slash + 1); }
  }
  field(100, n).copy(h, 0);
  octal(8, mode).copy(h, 100);
  octal(8, 0).copy(h, 108);              // uid
  octal(8, 0).copy(h, 116);              // gid
  octal(12, size).copy(h, 124);
  octal(12, 0).copy(h, 136);             // mtime
  h.fill(0x20, 148, 156);                // checksum placeholder: spaces
  field(1, type).copy(h, 156);
  field(100, '').copy(h, 157);           // linkname
  field(6, 'ustar\0').copy(h, 257);
  field(2, '00').copy(h, 263);
  field(32, '').copy(h, 265);            // uname
  field(32, '').copy(h, 297);            // gname
  field(8, 0).copy(h, 329);              // devmajor
  field(8, 0).copy(h, 337);              // devminor
  field(155, prefix).copy(h, 345);
  let sum = 0;
  for (const b of h) sum += b;
  field(6, sum.toString(8).padStart(6, '0')).copy(h, 148);
  h[154] = 0;
  h[155] = 0x20;
  return h;
}

/** gzip-compressed ustar archive from `{name, data, mode}` entries. */
export function tarGz(entries) {
  const blocks = [];
  for (const entry of entries) {
    const data = Buffer.isBuffer(entry.data) ? entry.data : Buffer.from(String(entry.data));
    blocks.push(tarHeader(entry.name, data.length, entry.mode ?? 0o644));
    blocks.push(data);
    const pad = (512 - (data.length % 512)) % 512;
    if (pad) blocks.push(Buffer.alloc(pad));
  }
  blocks.push(Buffer.alloc(1024)); // two zero blocks terminate the archive
  return gzipSync(Buffer.concat(blocks), { mtime: 0 });
}

// Re-exported for the generated default icon (a PNG encoder needs raw DEFLATE + CRC).
export { deflateRawSync };
