# Release checklist (manual runs)

CI (`.github/workflows/ci.yml`) already builds and tests all three desktop targets:
`cargo test --workspace`, a release `rendrix-runtime` build, and the CLI suite run on
Windows, macOS and Linux (Linux installs windowing/clipboard libs and software Vulkan).
What CI does **not** do is run the app on macOS/Linux hardware — the compatibility
matrix entry "macOS/Linux ❓ untested" stays until the steps below are executed and
recorded. Nothing here has been run yet.

## macOS manual run

Prerequisites: a Mac with Xcode command-line tools and Rust stable.

1. `npm install --no-audit --no-fund`
2. `npm run runtime` (release `rendrix-runtime` for macOS)
3. `node packages/cli/bin/rendrix.mjs doctor` — all rows green.
4. `node packages/cli/bin/rendrix.mjs run examples/app-counter` — window opens,
   counter increments on click, no console errors.
5. Headless: `RENDRIX_RUNTIME=engine/target/release/rendrix-runtime node scripts/e2e.mjs`
   (or the macOS equivalent path) — scripted expectations pass, screenshot written.
6. `node scripts/wpt.mjs` — record the pass rate in `docs/WPT.md` as the macOS row.
7. Package: `rendrix package examples/app-counter` — `.app` bundle produced;
   double-click launches outside a terminal.
8. Record: macOS version, chip (Intel/Apple Silicon), date, results.

## Linux manual run

Prerequisites: X11/Wayland dev libraries + software Vulkan
(`sudo apt-get install -y libx11-dev libxkbcommon-dev libwayland-dev libxcb1-dev mesa-vulkan-drivers`).

Steps 1–6 as on macOS (step 7: the raw binary launches; there is no installer).
Record: distro + version, Wayland vs X11, Mesa version, software vs hardware GL, results.

## Memory soak (all platforms)

Procedure (no soak has been run; no numbers below are results):

1. Build a soak page: a long scrollable document (a few hundred rows) plus a
   `setInterval` that appends/removes a row every second and a rAF-driven animation,
   so style, layout and paint stay warm.
2. Run headless with sampling, e.g.:
   `rendrix-runtime run soak.rdxapp --headless --script soak.json` in a loop, or the
   windowed app, while sampling process RSS every 60 s for **≥ 30 minutes**
   (PowerShell: `Get-Process rendrix-runtime | Select WorkingSet64`;
   Linux: `ps -o rss=`; macOS: `ps -o rss=` or Activity Monitor).
3. Expected (to be confirmed): RSS plateaus after warmup (fonts/atlas/text caches are
   bounded; detached DOM subtrees are GC'd about once a second). A steady climb of
   more than ~1 MB/min after the first 5 minutes is a leak — file it with the RSS
   series and the soak page attached.
4. Also run `cargo run -p rendrix-runtime -- bench 1000` before/after and compare the
   M0 `shapes`/`restyles`/`layouts` counters in [PERFORMANCE_AUDIT.md](PERFORMANCE_AUDIT.md).

## Results log

| Date | Platform / OS / GPU | Runtime sha | e2e | wpt | package | soak (30 min ΔRSS) | Notes |
|---|---|---|---|---|---|---|---|
| — | — | — | — | — | — | — | No manual runs recorded yet |
