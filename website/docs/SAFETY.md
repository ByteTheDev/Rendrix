# Safety and permissions

**Rendrix runs your application's code with the privileges of the user who starts it, plus a small set of
capability checks. It is not a sandbox for hostile code.** Only run applications you trust.

## What is enforced

| Capability | Default | How it is enforced |
|---|---|---|
| Network (`fetch` to `http(s)`) | **denied** | `permissions.network` in `rendrix.json`; otherwise `fetch` rejects with a `TypeError`. There is no per-host allow-list for `fetch`. WebSocket has one hardcoded exception: loopback (`localhost`, `127.0.0.1`, `::1`) is always allowed, even when `permissions.network` is false. |
| File reads | **denied** | Only through `rendrix.fs`; paths are canonicalised and must lie inside a directory listed in `permissions.fs_read`. Plain Web APIs cannot touch the filesystem. |
| File writes | **denied** | Same, with `permissions.fs_write`. |
| Relative `fetch`, images, fonts, stylesheets | bundled assets only | `DirAssets` and bundle assets reject `..` segments. |
| General process spawning, native add-ons, Node/Bun APIs | **do not exist** | There is no general script-facing spawn/add-on API. Two narrow host operations do reach the OS: `rendrix.update.install(path)` re-launches the current executable via `std::process::Command::spawn()`, and `rendrix.shell.openExternal()` asks the OS to open a URL. Neither accepts an arbitrary command. |
| Remote script/module loading | **do not exist** | The build refuses `http(s):`/`node:` imports; the module loader only reads the bundle. |

Covered by tests: `filesystem_access_is_permission_gated` (reads, writes, denied paths, denied network),
bundle trailer/format validation, and build-time rejection of Node built-ins and CommonJS.

## What is not enforced

* The execution budget is a liveness guard, not a sandbox: native QuickJS interrupts a task after 5 s and
  reports it as an error, and memory is capped at 1 GiB with a 2 MiB stack. Workers are separate JS
  contexts but run in the same process. On the experimental wasm target there is no execution-budget
  interrupt, so a runaway script hangs the worker (the page stays alive).
* QuickJS and native crates are not hardened against malicious inputs; treat untrusted images, fonts and
  HTML as a risk.
* Scripts can read the whole bundle and their own `localStorage` (a plain JSON file in the user data
  directory, in-memory only on the web target). Nothing is encrypted.
* A bundle appended to an executable is not integrity-checked by the runtime. Signing helpers are wired
  (`rendrix package --sign`, `--manifest`, `--manifest-key`, `--publish-url`, `--installer nsis`), but
  packages are unsigned by default and no signed macOS/Linux install has been run or recorded.
* `crypto.getRandomValues` uses the OS CSPRNG. `crypto.subtle.digest` (SHA-1/256/384/512) is available; the rest of `SubtleCrypto` does not exist.

## Reporting

Open a private security advisory on the repository. Please include the bundle or script that reproduces
the behaviour.
