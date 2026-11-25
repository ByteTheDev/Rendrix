# Accessibility plan

## Status

**Bridge implemented, manual verification pending.** `rendrix-js` builds an AccessKit tree from the DOM
(role from tag/`role`, name from `aria-label`/`aria-labelledby`/`<label>`/text content, state from
`disabled`/`checked`/`aria-*`, value and selection for text controls, bounds from layout). `accesskit_winit`
bridges it to UIA on Windows, NSAccessibility on macOS and AT-SPI on Linux, and routes Click/Focus/Blur/
SetValue/ReplaceSelectedText actions back through `Session` entry points. The tree is pushed on repainted
frames only, so idle frames stay at zero work. Unit and integration tests assert tree shape
(`tests/platform.rs`, `tests/regression.rs`).

`matchMedia('(prefers-reduced-motion)')` and `(prefers-contrast)` read the environment variables
`RENDRIX_REDUCED_MOTION` and `RENDRIX_CONTRAST_MORE` on all native targets; real per-OS preference queries
are not implemented on any platform yet. Keyboard use works: Tab order,
Enter/Space activation, text editing, caret and selection, with focus rings drawn through normal CSS
`:focus` / `:focus-visible`.

**No manual assistive-technology pass has been performed or recorded.** Do not claim screen-reader support
until the procedure in [A11Y_CHECKLIST.md](A11Y_CHECKLIST.md) has been executed and logged.

## Remaining work

1. **Manual verification.** Narrator / NVDA on Windows, VoiceOver on macOS and Orca on Linux, following
   [A11Y_CHECKLIST.md](A11Y_CHECKLIST.md). Record date, build, OS/AT versions and pass/fail per step.
2. **Preference parity.** Implement real per-OS reduced-motion and contrast queries on Windows/macOS/Linux,
   and push browser preferences through on the wasm target, instead of relying on environment overrides.
3. **Coverage.** Expand role/state mapping to the newer controls (date picker popup, validation messages,
   3D scene hosts), expose live regions and verify reading order against complex layout (tables, floats).
4. **Regression.** Keep tree-shape and AT-action tests in CI; add screenshot-level focus-visibility checks.
