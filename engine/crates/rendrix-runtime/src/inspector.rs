//! Headless inspector (D3): an on-demand JSON snapshot of live engine state.
//!
//! [`snapshot`] reads PUBLIC engine APIs only and never mutates the session: it is safe to call
//! from a debugger command, a `--inspect` dump path, or a test. It performs no work unless
//! called, so production frames pay zero cost (there is no overlay, no polling, no per-frame
//! hook — the overlay concept from the plan is intentionally a snapshot tool, not an in-frame
//! view, so measured frame budgets are never perturbed).
//!
//! Contents: viewport, [`EngineStats`](rendrix_core::EngineStats) (frames, restyles, layouts,
//! style/layout/paint micros, `shape_count`/`cache_hits`/`cache_len`), display-list length, a
//! bounded node table (tag, id/class attrs, border-box rect, `display`) with the total node
//! count, focus, the JS engine name, JS heap bytes, pending timer/frame work, and the most
//! recent console entries (errors included) from the host console log.

use crate::session::Session;
use serde_json::{json, Value};

/// Maximum nodes described individually; `node_count` always reports the full total.
pub const MAX_NODES: usize = 500;
/// Maximum console entries (most recent) included.
pub const MAX_CONSOLE: usize = 20;

pub fn snapshot(session: &Session) -> Value {
    let e = session.engine.borrow();
    let stats = &e.stats;
    let root = e.doc.root();
    let all = e.doc.depth_first(root);
    let node_count = all.len();
    let mut element_count = 0usize;
    let mut nodes = Vec::new();
    for id in all {
        let Some(tag) = e.doc.tag(id).map(str::to_string) else {
            continue;
        };
        element_count += 1;
        if nodes.len() >= MAX_NODES {
            continue;
        }
        nodes.push(json!({
            "tag": tag,
            "id": e.doc.get_attr(id, "id").unwrap_or(""),
            "class": e.doc.get_attr(id, "class").unwrap_or(""),
            "rect": e.element_rect(id),
            "display": e.computed_value(id, "display"),
        }));
    }
    let (vw, vh) = e.viewport;
    let display_items = e.display_list().items.len();
    let focused = e.doc.focused();
    let tick = session.js.tick_info();
    let console: Vec<Value> = session
        .js
        .host
        .borrow()
        .console
        .iter()
        .rev()
        .take(MAX_CONSOLE)
        .rev()
        .map(|(level, msg)| json!({ "level": level, "message": msg }))
        .collect();
    let errors: Vec<&Value> = console.iter().filter(|c| c["level"] == "error").collect();
    json!({
        "app": session.bundle_name,
        "viewport": { "width": vw, "height": vh, "scale": e.scale, "dark": e.dark },
        "stats": {
            "frames": stats.frames,
            "restyles": stats.restyles,
            "layouts": stats.layouts,
            "full_restyles": stats.full_restyles,
            "last_style_micros": stats.last_style_micros,
            "last_layout_micros": stats.last_layout_micros,
            "last_paint_micros": stats.last_paint_micros,
            "shape_count": stats.shape_count,
            "cache_hits": stats.cache_hits,
            "cache_len": stats.cache_len,
        },
        "display_items": display_items,
        "node_count": node_count,
        "element_count": element_count,
        "nodes_truncated": element_count > nodes.len(),
        "nodes": nodes,
        "focused": focused,
        "js": {
            "engine": session.js.engine_name(),
            "memory_bytes": session.js.memory_usage(),
            "wants_frame": tick.wants_frame,
            "next_timer_ms": tick.next_timer_ms,
            "pending_io": tick.pending_io,
        },
        "console": console,
        "recent_errors": errors,
    })
}
