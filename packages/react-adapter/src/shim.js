// Rendrix DOM compatibility shims for the React adapter (adapter-owned, not engine-owned).
//
// The Rendrix prelude intentionally exposes a small DOM subset. Unlike `solid-js/h`, the
// React production builds used here only touch widely-available globals (`setTimeout`,
// `clearTimeout`, `setImmediate`, `queueMicrotask`, `performance.now`, `console`), and the
// scheduler falls back to `setTimeout(..., 0)` when `MessageChannel` is absent — so today
// this file installs nothing. It is still imported first by `src/index.js` (ES module
// evaluation is depth-first in source order) so future React versions that need a
// no-op-compatible stub get one here without touching the engine.
