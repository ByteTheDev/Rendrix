#!/usr/bin/env node
// The `rendrix` command line. Applications are TypeScript/TSX projects that run on Rendrix's own
// runtime (DOM + CSS + layout + GPU renderer). See `rendrix help`.

import path from 'node:path';
import { createRequire } from 'node:module';
import { BuildError } from '../src/app/build.mjs';
import { build, check, dev, doctor, init, pack, render, run } from '../src/app/commands.mjs';

const require = createRequire(import.meta.url);
const VERSION = require('../package.json').version;

const HELP = `rendrix ${VERSION}: build native apps with TypeScript, DOM and CSS (no Chromium, Electron or WebView)

Usage:
  rendrix init <dir> [--name N] [--template app]   create a new project
  rendrix dev [dir]                                 build, run, and rebuild on every change
  rendrix build [dir] [--out file] [--no-typecheck] [--check-js] compile to a .rdxapp bundle
  rendrix run [dir]                                 build and run once
  rendrix check [dir] [--check-js]                  type-check only
  rendrix package [dir] [--out file] [--runtime P] [--target TRIPLE] [--icon FILE] [--installer none|nsis|zip|tarball] [--sign] [--manifest file] [--manifest-key key.pem] [--publish-url url] [--app-version x.y.z]
  build a standalone executable, .app bundle (macOS) or tarball (Linux)
  rendrix render <page.html|dir> -o out.png|out.pdf|frames/ [--size WxH] [--scale S] [--frames N] [--frame-ms MS] [--script steps.json]
  render a page headlessly (PNG, PDF, or a PNG frame sequence for the /video pipeline)
  rendrix doctor [dir]                              verify the toolchain, GPU and runtime
  rendrix help | version

Options (any command that takes [dir] defaults to the current directory):
  --target TRIPLE                                    package for another OS/arch (e.g. aarch64-apple-darwin,
                                                     x86_64-unknown-linux-gnu); needs --runtime or RENDRIX_HOME
  --icon FILE                                        square .png or .icns used for the macOS/Linux app icon
  --headless / --screenshot <png> / --script <json>   passed through to the runtime by \`run\`
`;

function parse(argv) {
  const opts = { _: [], runtimeArgs: [] };
  for (let i = 0; i < argv.length; i++) {
    const a = argv[i];
    if (a === '--no-typecheck') opts.typecheck = false;
    else if (a === '--check-js') opts.checkJs = true;
    else if (a === '--no-hot') opts.hot = false;
    else if (a === '--hot') opts.hot = true;
    else if (a === '--out' || a === '-o') opts.out = argv[++i];
    else if (a === '--frames') opts.frames = argv[++i];
    else if (a === '--frame-ms') opts.frameMs = argv[++i];
    else if (a === '--name') opts.name = argv[++i];
    else if (a === '--template') opts.template = argv[++i];
    else if (a === '--runtime') opts.runtime = argv[++i];
    else if (a === '--target') opts.target = argv[++i];
    else if (a === '--icon') opts.icon = argv[++i];
    else if (a === '--installer') opts.installer = argv[++i];
    else if (a === '--sign') opts.sign = true;
    else if (a === '--manifest') opts.manifest = argv[++i];
    else if (a === '--manifest-key') opts.manifestKey = argv[++i];
    else if (a === '--publish-url') opts.publishUrl = argv[++i];
    else if (a === '--app-version') opts.appVersion = argv[++i];
    else if (a === '--headless') opts.runtimeArgs.push(a);
    else if (['--screenshot', '--script', '--size', '--scale'].includes(a)) {
      const v = argv[++i];
      opts.runtimeArgs.push(a, v);
      if (a === '--size') opts.size = v;
      else if (a === '--scale') opts.scale = v;
      else if (a === '--script') opts.script = v;
    }
    else if (a.startsWith('--')) throw new BuildError(`unknown option ${a}. Try \`rendrix help\`.`);
    else opts._.push(a);
  }
  return opts;
}

async function main() {
  const argv = process.argv.slice(2);
  const cmd = argv[0] && !argv[0].startsWith('-') ? argv.shift() : 'help';
  const opts = parse(argv);
  const dir = path.resolve(opts._[0] || '.');
  switch (cmd) {
    case 'init': if (!opts._[0]) throw new BuildError('usage: rendrix init <dir>'); return init(opts._[0], opts);
    case 'build': await build(dir, opts); return 0;
    case 'check': await check(dir, opts); return 0;
    case 'run': return run(dir, opts);
    case 'dev': await dev(dir, opts); return 0;
    case 'package': await pack(dir, opts); return 0;
    case 'render': if (!opts._[0]) throw new BuildError('usage: rendrix render <page.html|dir> -o out.png|out.pdf|frames/'); await render(opts._[0], opts); return 0;
    case 'doctor': return doctor(dir);
    case 'version': case '--version': console.log(VERSION); return 0;
    case 'help': default: console.log(HELP); return cmd === 'help' ? 0 : 2;
  }
}

main().then(code => process.exit(code || 0), e => {
  console.error(e instanceof BuildError ? `error: ${e.message}` : e.stack || String(e));
  process.exit(1);
});
