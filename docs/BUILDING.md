# Building and running

## Requirements

* Rust (stable) and a C/C++ toolchain: MSVC Build Tools on Windows, Xcode command line tools on macOS,
  `build-essential` plus X11/Wayland development headers on Linux.
* Node.js 18+ (the CLI and the TypeScript compiler). Bun or npm both work for installing.
* A GPU with a Vulkan, Metal or DirectX 12 driver. The runtime picks the adapter itself;
  `rendrix doctor` prints which one.

Only Windows has been exercised so far (see [COMPATIBILITY.md](COMPATIBILITY.md)); specifically x64.
Windows ARM64 and x86 are compile-checked in CI and cross-packaged by tests, but have not been run on
hardware here.

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

### Cross-target packaging

Packaging is target-driven, not host-driven:

```sh
rendrix package --target x86_64-unknown-linux-gnu --runtime ./runtimes/linux-x64/rendrix-runtime
rendrix package --target aarch64-apple-darwin --runtime ./runtimes/darwin-arm64/rendrix-runtime
rendrix package --target i686-pc-windows-msvc --runtime ./runtimes/win32-x86/rendrix-runtime.exe  # 32-bit Windows
rendrix package --target armv7-unknown-linux-gnueabihf --runtime ./runtimes/linux-armv7/rendrix-runtime  # Raspberry Pi (32-bit)
```

Supported target keys are `win32-x64`, `win32-arm64`, `win32-x86`, `darwin-x64`, `darwin-arm64`,
`linux-x64`, `linux-x86`, `linux-arm64`, `linux-armv7` and `linux-riscv64` (Rust triples and short
aliases such as `macos-intel` also work). Windows x86 is the 32-bit target:
`i686-pc-windows-msvc` / `i686-pc-windows-gnu` (aliases `x86` / `ia32`) map to the `win32-x86` key,
alongside `win32-x64` and `win32-arm64`. Linux adds three packaging targets beyond x64/arm64: 32-bit
x86 (`i686-unknown-linux-gnu`), 32-bit ARM (`armv7-unknown-linux-gnueabihf`, aliases `armv7` /
`armhf`, the Raspberry Pi 32-bit target) and 64-bit RISC-V (`riscv64gc-unknown-linux-gnu`, alias
`riscv64`); Node on a 32-bit ARM Linux host reports arch `arm`, which the CLI detects as
`linux-armv7`. A cross-target build never picks up a local `engine/target` runtime, because that
binary is host-only — pass `--runtime` or place binaries under
`$RENDRIX_HOME/runtime/<platform>-<arch>/`.

Building the engine for `i686-pc-windows-msvc` needs **libclang (LLVM)** on the build machine: `rquickjs-sys`
ships no pregenerated bindings for that triple, so the `bindgen` feature is enabled automatically for 32-bit
Windows. Set `LIBCLANG_PATH` if libclang is not on the default search path.

Cross-compiling the engine for the extra Linux architectures needs the matching cross C toolchain, because
`rquickjs` (QuickJS) and `ring` compile C code for the target; `rquickjs-sys` 0.14 does ship pregenerated
bindings for all three triples, so no libclang step is involved. On Debian/Ubuntu:

```sh
sudo apt-get install gcc-arm-linux-gnueabihf libudev-dev  # or gcc-i686-linux-gnu / gcc-riscv64-linux-gnu
export CC_armv7_unknown_linux_gnueabihf=arm-linux-gnueabihf-gcc
export CARGO_TARGET_ARMV7_UNKNOWN_LINUX_GNUEABIHF_LINKER=arm-linux-gnueabihf-gcc
export PKG_CONFIG_ALLOW_CROSS=1                          # gilrs' libudev-sys build script probes pkg-config
cargo check --target armv7-unknown-linux-gnueabihf -p rendrix-runtime --manifest-path engine/Cargo.toml
```

`cargo check` runs build scripts but never links, so the host's pkg-config metadata satisfies libudev.
On a Windows or macOS host there is no practical way to install the Linux cross C compilers, so those
checks are owned by the `linux-cross-compile` CI job (`.github/workflows/ci.yml`).

macOS produces `<name>.app/Contents/{MacOS,Resources,Info.plist,PkgInfo}` and a zipped `.app` for
distribution; `--sign` signs the bundle and the notarization step submits the zip and staples the app.
Linux produces a `.tar.gz` with `bin/`, a freedesktop `.desktop` entry and a hicolor icon. Pass a square
`--icon <file>` (`.png` or `.icns`) to override the generated default icon on either platform. NSIS
installers remain Windows-only.

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
TypeScript compiler version from `package.json`. Release profile: fat LTO, one codegen
unit per crate, stripped symbols, `opt-level = 3`; see [PERF_BINARY_SIZE.md](PERF_BINARY_SIZE.md).
Dev/test builds are unchanged.
