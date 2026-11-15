//! M0 micro-benches for the JS boundary: `call_global` and the `__native` round-trip.
//!
//! Dev-only harness (`criterion`, never shipped). The round-trip number isolates the
//! Val marshalling + host dispatch cost the A4 worker bridge and P5 pooling must respect.

use criterion::{black_box, criterion_group, criterion_main, Criterion};
use rendrix_core::{Engine, MapAssets};
use rendrix_dom::Document;
use rendrix_js::{JsRuntime, Permissions, Val};
use std::cell::RefCell;
use std::rc::Rc;

fn runtime() -> JsRuntime {
    let doc = Document::parse("<html><head></head><body><div id=a>hi</div></body></html>");
    let mut engine = Engine::with_document(doc, Box::new(MapAssets::default()));
    engine.set_viewport(400.0, 300.0, 1.0);
    let engine = Rc::new(RefCell::new(engine));
    let rt = JsRuntime::new(engine, "bench", Permissions::allow_all()).expect("runtime");
    rt.host.borrow_mut().echo_console = false;
    rt.host.borrow_mut().disable_storage_persistence();
    rt
}

/// Pure `call_global` path: no host crossing (baseline for the round-trip below).
fn bench_call_global(c: &mut Criterion) {
    let mut rt = runtime();
    rt.run_script("bench-setup", "globalThis.add42 = x => x + 42;").expect("setup");
    c.bench_function("js/call_global_pure", |b| {
        b.iter(|| black_box(rt.eval(black_box("(globalThis.add42(1))"))));
    });
}

/// Full round-trip: JS → prelude → `__native` op → Rust host → Val back to JS.
fn bench_native_round_trip(c: &mut Criterion) {
    let mut rt = runtime();
    c.bench_function("js/native_round_trip_document_root", |b| {
        // `document.documentElement` crosses the boundary on every access.
        b.iter(|| black_box(rt.eval(black_box("(document.documentElement.tagName)"))));
    });
}

/// A DOM read that fans out to several host ops (shape of real framework code).
fn bench_native_dom_read(c: &mut Criterion) {
    let mut rt = runtime();
    c.bench_function("js/native_dom_read_fanout", |b| {
        b.iter(|| {
            black_box(rt.eval(black_box(
                "(document.getElementById('a').textContent + document.body.childNodes.length)",
            )))
        });
    });
}

/// Async task pump cost: resolved promise + one expired timer.
fn bench_task_pump(c: &mut Criterion) {
    let mut rt = runtime();
    c.bench_function("js/task_pump_microtask", |b| {
        b.iter(|| {
            rt.run_script("bench-task", "Promise.resolve(1).then(() => {});").expect("task");
            black_box(Val::Undefined);
        });
    });
}

criterion_group!(benches, bench_call_global, bench_native_round_trip, bench_native_dom_read, bench_task_pump);
criterion_main!(benches);
