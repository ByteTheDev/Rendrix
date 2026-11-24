import {writeFile} from 'node:fs/promises';
import {spawnSync} from 'node:child_process';
import path from 'node:path';
function literal(value) {
  if (!value || /[\r\n"$]/.test(value)) throw new Error('Invalid NSIS metadata or path');
  return value;
}
// Pick the install root for the target architecture. Only fixed NSIS constants are
// returned here, so the value can be interpolated into the script safely.
function nsisInstallDir(arch) {
  const value = String(arch || '').trim().toLowerCase();
  if (value === 'x86' || value === 'ia32' || value === 'i386' || value === 'i486' || value === 'i586' || value === 'i686') {
    // $PROGRAMFILES32 is unambiguously the 32-bit Program Files directory. For a
    // 32-bit installer $PROGRAMFILES already points there, but naming it explicitly
    // keeps the target architecture clear regardless of the installer's bitness.
    return '$PROGRAMFILES32';
  }
  if (value === 'x64' || value === 'x86_64' || value === 'amd64' || value === 'arm64' || value === 'aarch64') {
    // 64-bit apps (x64 and arm64) must not be installed into Program Files (x86).
    return '$PROGRAMFILES64';
  }
  // No (or unrecognized) arch: keep the historical $PROGRAMFILES default so
  // existing callers that do not pass `arch` retain their previous install location.
  return '$PROGRAMFILES';
}
export function nsisScript({name, version, executable, output, publishUrl, certificateSubjectName, arch}) {
  name = literal(name); version = literal(version); executable = literal(path.resolve(executable)); output = literal(path.resolve(output));
  const exe = literal(path.basename(executable));
  const safe = value => value ? String(value).replace(/[\r\n"$]+/g, '_') : '(unset)';
  const installDir = nsisInstallDir(arch);
  return `Unicode true
; Rendrix NSIS installer for ${name} ${version}
; Target architecture: ${safe(arch)}
; Signing: rendrix package --installer nsis --sign (RENDRIX_SIGN_COMMAND, or RENDRIX_CERT_FILE on Windows)
; Updates: manual publish — upload the installer, the standalone exe, and the SHA-256 manifest
; (rendrix package --manifest manifest.json [--manifest-key key.pem]) to publishUrl ${safe(publishUrl)}.
; The app checks that manifest with rendrix.update.check(feedUrl), verifies the download hash,
; and restarts into the new binary with update.install() (quitAndInstall semantics). There is no
; Electron autoUpdater here; do not reference autoUpdater.setFeedURL in installer scripts.
Name "${name} ${version}"
OutFile "${output}"
InstallDir "${installDir}\\${name}"
RequestExecutionLevel admin
Section
  SetOutPath "$INSTDIR"
  File "${executable}"
  CreateDirectory "$SMPROGRAMS\\${name}"
  CreateShortcut "$SMPROGRAMS\\${name}\\${name}.lnk" "$INSTDIR\\${exe}"
  WriteUninstaller "$INSTDIR\\Uninstall.exe"
SectionEnd
Section "Uninstall"
  Delete "$SMPROGRAMS\\${name}\\${name}.lnk"
  RMDir "$SMPROGRAMS\\${name}"
  Delete "$INSTDIR\\${exe}"
  Delete "$INSTDIR\\Uninstall.exe"
  RMDir "$INSTDIR"
SectionEnd
`;
}
export async function buildNsis(options, scriptPath) {
  await writeFile(scriptPath, nsisScript(options));
  const result = spawnSync('makensis', [scriptPath], {encoding:'utf8'});
  if (result.error?.code === 'ENOENT') throw new Error(`makensis is not installed; script written to ${scriptPath}`);
  if (result.error || result.status !== 0) throw new Error(`makensis failed: ${result.stderr || result.stdout || result.error?.message}`);
  return result.stdout;
}
