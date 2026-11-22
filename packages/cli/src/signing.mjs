import { readFile, writeFile } from 'node:fs/promises';
import { createHash, createPrivateKey, sign } from 'node:crypto';
import { spawn, spawnSync } from 'node:child_process';
import path from 'node:path';
import fs from 'node:fs';

export async function writeManifest(files, destination, keyPath) {
  const artifacts = {};
  for (const file of files) artifacts[path.basename(file)] = createHash('sha256').update(await readFile(file)).digest('hex');
  const data = Buffer.from(JSON.stringify({ algorithm: 'sha256', artifacts }, null, 2) + '\n');
  await writeFile(destination, data);
  if (keyPath) {
    const key = createPrivateKey(await readFile(keyPath));
    if (key.asymmetricKeyType !== 'ed25519') throw new Error('Manifest signing key must be Ed25519');
    await writeFile(destination + '.sig', sign(null, data, key).toString('base64') + '\n');
  }
  return destination;
}

// Split a signing command into arguments without a shell. Quotes are honoured; environment
// variables and shell operators are NOT expanded, so a configured command cannot run
// arbitrary extra processes. Prefer passing { command, args } from resolveSignCommand().
export function splitCommand(input) {
  const out = [];
  let current = '', quote = null;
  for (let i = 0; i < input.length; i++) {
    const c = input[i];
    if (quote) {
      if (c === quote) quote = null;
      // Only \" and \\ are escapes; a bare backslash is part of a Windows path.
      else if (c === '\\' && quote === '"' && (input[i + 1] === '"' || input[i + 1] === '\\')) current += input[++i];
      else current += c;
      continue;
    }
    if (c === '"' || c === "'") { quote = c; continue; }
    if (/\s/.test(c)) { if (current) { out.push(current); current = ''; } continue; }
    current += c;
  }
  if (quote) throw new Error('unterminated quote in RENDRIX_SIGN_COMMAND');
  if (current) out.push(current);
  return out;
}

function toInvocation(command) {
  if (typeof command === 'string') {
    const parts = splitCommand(command);
    return { command: parts.shift(), args: parts };
  }
  if (!command || !command.command) throw new Error('invalid signing command');
  return { command: command.command, args: [...(command.args || [])] };
}

// Windows SDK roots are derived from environment variables so this works when Windows is
// installed on a non-C: drive or when the developer prompt has not set its variables.
export function signtoolRoots() {
  const roots = [];
  const push = p => { if (p && !roots.includes(p)) roots.push(p); };
  if (process.env.WINDOWSSDKDIR) push(path.join(process.env.WINDOWSSDKDIR, 'bin'));
  const bases = [process.env['ProgramFiles(x86)'], process.env.ProgramFiles, 'C:\\Program Files (x86)', 'C:\\Program Files'];
  for (const base of bases) {
    if (!base) continue;
    push(path.join(base, 'Windows Kits', '10', 'bin'));
    push(path.join(base, 'Windows Kits', '8.1', 'bin'));
  }
  return roots;
}

export function signtoolCandidates(roots, arch = process.arch) {
  const order = arch === 'ia32' ? ['x86', 'x64', 'arm64'] : arch === 'arm64' ? ['arm64', 'x64', 'x86'] : ['x64', 'x86', 'arm64'];
  const candidates = [];
  const add = dir => { for (const a of order) candidates.push(path.join(dir, a, 'signtool.exe')); };
  for (const root of roots) {
    add(root); // flat layout (SDK 8.1: <root>/x64/signtool.exe)
    let versions = [];
    try { versions = fs.readdirSync(root).sort().reverse(); } catch { continue; }
    for (const version of versions) {
      const dir = path.join(root, version);
      try { if (fs.statSync(dir).isDirectory()) add(dir); } catch { /* not a SDK version dir */ }
    }
  }
  return [...new Set(candidates)];
}

// Locate signtool.exe on Windows. Returns an absolute path, a PATH command name, or null.
export function findSigntool() {
  if (process.platform !== 'win32') return null;
  try {
    const probe = spawnSync('signtool', ['/?'], { stdio: 'ignore', windowsHide: true });
    if (probe.status === 0 || probe.status === 1) return 'signtool';
  } catch { /* not on PATH */ }
  const arch = process.env.RENDRIX_SIGNTOOL_ARCH || process.arch;
  return signtoolCandidates(signtoolRoots(), arch).find(p => fs.existsSync(p)) || null;
}

export function findCodesign() {
  if (process.platform !== 'darwin') return null;
  try { if (spawnSync('codesign', ['--version'], { stdio: 'ignore' }).status === 0) return 'codesign'; } catch { /* absent */ }
  return null;
}

export function findNotarytool() {
  if (process.platform !== 'darwin') return null;
  try { if (spawnSync('xcrun', ['notarytool', '--version'], { stdio: 'ignore' }).status === 0) return 'xcrun'; } catch { /* absent */ }
  return null;
}

export function findGpg() {
  if (process.platform !== 'linux') return null;
  try { if (spawnSync('gpg', ['--version'], { stdio: 'ignore' }).status === 0) return 'gpg'; } catch { /* absent */ }
  return null;
}

// Resolve the signing invocation. Returns { command, args } or null (unsigned).
// Precedence: RENDRIX_SIGN_COMMAND, then platform-specific environment shortcuts.
export function resolveSignCommand() {
  if (process.env.RENDRIX_SIGN_COMMAND) {
    const invocation = toInvocation(process.env.RENDRIX_SIGN_COMMAND);
    return invocation.command ? invocation : null;
  }
  if (process.platform === 'win32' && process.env.RENDRIX_CERT_FILE) {
    const tool = findSigntool();
    if (!tool) return null;
    const password = process.env.RENDRIX_CERT_PASSWORD || '';
    const args = ['sign', '/f', process.env.RENDRIX_CERT_FILE];
    if (password) args.push('/p', password);
    args.push('/tr', 'http://timestamp.digicert.com', '/td', 'sha256', '/fd', 'sha256');
    return { command: tool, args };
  }
  if (process.platform === 'darwin' && process.env.RENDRIX_CODESIGN_IDENTITY) {
    return { command: 'codesign', args: ['--force', '--deep', '--sign', process.env.RENDRIX_CODESIGN_IDENTITY, '--timestamp', '--options', 'runtime'] };
  }
  if (process.platform === 'linux' && process.env.RENDRIX_GPG_KEY) {
    return { command: 'gpg', args: ['--local-user', process.env.RENDRIX_GPG_KEY, '--detach-sign', '--armor'] };
  }
  return null;
}

// Notarize the zipped .app (Apple requires a zip for submission) and staple the bundle.
// `file` alone is accepted for backwards compatibility with raw-binary notarization.
export async function notarizeArtifact(file, options = {}) {
  if (process.platform !== 'darwin') return false;
  const appleId = process.env.APPLE_ID;
  const teamId = process.env.APPLE_TEAM_ID;
  const appPassword = process.env.APPLE_APP_PASSWORD;
  if (!appleId || !teamId || !appPassword) {
    console.log('Notarization skipped: set APPLE_ID, APPLE_TEAM_ID and APPLE_APP_PASSWORD to notarize.');
    return false;
  }
  if (!findNotarytool()) { console.log('Notarization skipped: xcrun notarytool not found.'); return false; }
  const zip = options.zip || file;
  const app = options.app || file;
  const submit = spawnSync('xcrun', ['notarytool', 'submit', path.resolve(zip), '--apple-id', appleId, '--team-id', teamId, '--password', appPassword, '--wait'], { stdio: 'inherit' });
  if (submit.status !== 0) throw new Error('Notarization submission failed.');
  if (/\.app$/i.test(app)) {
    const staple = spawnSync('xcrun', ['stapler', 'staple', path.resolve(app)], { stdio: 'inherit' });
    if (staple.status !== 0) console.warn('Notarization succeeded but stapling the .app failed; the ticket will be fetched on first launch.');
  } else {
    console.warn('Notarized archive, but no .app bundle was supplied to staple.');
  }
  console.log(`Notarized: ${path.resolve(zip)}`);
  return true;
}

// No shell is involved on any platform: arguments are passed straight to the tool. This also
// makes signing work when /bin/sh is unavailable and avoids quoting bugs on Windows paths.
export async function signArtifact(file, command = resolveSignCommand()) {
  if (!command) {
    console.log('Artifact is unsigned (set RENDRIX_SIGN_COMMAND, or RENDRIX_CERT_FILE on Windows / RENDRIX_CODESIGN_IDENTITY on macOS).');
    return false;
  }
  const invocation = toInvocation(command);
  if (!invocation.command) throw new Error('signing requested but no signing command is configured');
  const child = spawn(invocation.command, [...invocation.args, path.resolve(file)], { stdio: 'inherit', windowsHide: true });
  const code = await new Promise((resolve, reject) => { child.once('error', reject); child.once('close', resolve); });
  if (code !== 0) throw new Error(`Signing command failed (${invocation.command} exited ${code}).`);
  return true;
}
