# Building and running

## Requirements

* Rust (stable) and a C/C++ toolchain: MSVC Build Tools on Windows, Xcode command line tools on macOS,
  `build-essential` plus X11/Wayland development headers on Linux.
* Node.js 18+ (the CLI and the TypeScript compiler). Bun or npm both work for installing.
* A GPU with a Vulkan, Metal or DirectX 12 driver. The runtime picks the adapter itself;
  `rendrix doctor` prints which one.

Only Windows has been exercised so far (see [COMPATIBILITY.md](COMPATIBILITY.md)).

## Build the runtime

```sh
cd engine
cargo test --workspace                      # ~375 Rust tests (GPU tests skip themselves with no adapter)
cargo build --release -p rendrix-runtime    # engine/target/release/rendrix-runtime[.exe]
```

The CLI finds the runtime through `RENDRIX_RUNTIME`, then `engine/target/{release,debug}`, then
`$RENDRIX_HOME/runtime/<platform>-<arch>/`.

## Create and run an application

```sh
npm install                                  # once, at the repository root (installs TypeScript)
node packages/cli/bin/rendrix.mjs init my-app
cd my-app
rendrix dev        # builds, opens a native window, hot-reloads changed modules on the live DOM
rendrix build      # type-check + compile to dist/my-app.rdxapp
rendrix run        # build and run once
rendrix check      # type-check only
rendrix package    # dist/my-app.exe: one standalone executable, no Node/Bun/browser needed
rendrix render     # headless page → PNG, PDF or a PNG frame sequence
rendrix doctor     # Node, TypeScript, runtime, GPU adapter, project config
```

A project is `rendrix.json` (name, window size/title, permissions), an `index.html` (with `<link
rel="stylesheet">` and `<script type="module" src="…ts|tsx">`), `src/**`, and any assets. `tsconfig.json`
selects `"jsx": "react-jsx"` and `"jsxImportSource": "rendrix"`.

`rendrix dev` rebuilds and hot-swaps changed modules into a fresh JS runtime on the **live DOM**. State
preservation is script-cooperative: globals and DOM state survive when the script re-initialises them, and a
failed update keeps the last-good tree with the error logged. Use `--no-hot` to restore restart-on-change.
The optional `--inspect out.json` runtime flag dumps a DOM/style/layout/engine-stats snapshot; there is no
overlay inspector UI yet.

## Verifying a build

```sh
npm test                 # cargo test --workspace + the CLI test suite (runs bundles in the real runtime)
npm run e2e              # builds examples/app-counter, replays scripted clicks/resizes headlessly on the GPU,
                         # fails if any expectation breaks, writes screenshots to .rendrix-shots/
npm run wpt              # curated CSS layout conformance subset against the headless runtime
npm run bench            # frame-pipeline timings on a 1,000-item list (release build)

# Headless rendering of a standalone page (no project needed):
rendrix render page.html -o out.png        # also out.pdf, or frames/ for a PNG sequence
# Optional experimental wasm target (needs wasm32-unknown-unknown + wasm-bindgen-cli):
cargo check --target wasm32-unknown-unknown --manifest-path engine/Cargo.toml -p rendrix-web
```

Runtime flags useful for debugging: `--headless --screenshot out.png`, `--script steps.json`
(steps: `click`, `move`, `type`, `key`, `wheel`, `resize`, `wait`, `settle`, `eval`, `expect`, `screenshot`),
`--size WxH`, `--scale S`. Environment: `RENDRIX_DUMP_FRAME=file.png` writes the frame the window presents,
`RENDRIX_TRACE_EVENTS=1` logs window events.

## Reproducibility

`engine/Cargo.lock` is committed; Rust dependencies resolve from it. The JavaScript side uses the
TypeScript compiler version from `package.json`. Release profile: thin LTO, `opt-level = 3`.
