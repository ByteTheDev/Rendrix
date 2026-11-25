# WPT subset conformance

Curated CSS layout subset run against the real headless runtime
(`node scripts/wpt.mjs`). It covers the layout areas this project implements
(tables, floats, multi-column, grid, flexbox, block flow, positioning) — it is **not**
the full web-platform-tests suite, and the rate below tracks *this subset only*.
No browser parity is claimed.

Run it: `node scripts/wpt.mjs` (needs a built runtime: `npm run runtime`).
History: `scripts/wpt-history.json` (appended per run, last 50 kept).

## Latest recorded run (2026-11-15): 15/15 (100.0%)

Only one run has been recorded in `scripts/wpt-history.json` so far; this is that run, not a repeated result.

|  | Case | Checks |
|---|---|---|
| ✅ | table-2x2-grid | 4/4 |
| ✅ | table-sections-caption-col | 4/4 |
| ✅ | table-explicit-roles | 2/2 |
| ✅ | table-ua-defaults | 3/3 |
| ✅ | float-left-pin | 3/3 |
| ✅ | float-right-pin | 2/2 |
| ✅ | clear-below-float | 2/2 |
| ✅ | float-clear-computed | 2/2 |
| ✅ | multicol-count-2 | 5/5 |
| ✅ | multicol-span-all | 2/2 |
| ✅ | multicol-columns-shorthand | 2/2 |
| ✅ | guard-flex-row | 2/2 |
| ✅ | guard-grid-columns | 2/2 |
| ✅ | guard-block-percent-padding | 2/2 |
| ✅ | guard-absolute-position | 2/2 |

## Recorded runs (up to 12 most recent; currently 1)

| Date | Pass |
|---|---|
| 2026-11-15 | 15/15 (100.0%) |
