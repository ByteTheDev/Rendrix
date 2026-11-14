//! M0 micro-benches for the core engine pipeline (public API only).
//!
//! Dev-only harness (`criterion`, never shipped). `build_root` itself is crate-private,
//! so these benches drive it through `Engine::update`, which is the path P1 must improve:
//! identical screenshots with smaller medians here is the P1 acceptance signal.

use criterion::{black_box, criterion_group, criterion_main, Criterion};
use rendrix_core::{Engine, MapAssets};
use rendrix_dom::Document;

const ITEMS: usize = 1000;

fn bench_document(items: usize) -> Document {
    let mut body = String::from("<div id=list>");
    for i in 0..items {
        body.push_str(&format!(
            "<div class=item id=i{i}><span class=idx>{i}</span>\
             <span class=label>Item number {i} with some descriptive text</span>\
             <span class=tag>tag {}</span><button>Open</button></div>",
            i % 7
        ));
    }
    body.push_str("</div>");
    let src = format!(
        "<html><head><style>\
        body{{margin:0;background:#101010;color:#ddd;font:14px sans-serif}}\
        #list{{padding:8px;display:flex;flex-direction:column;gap:4px}}\
        .item{{display:flex;align-items:center;gap:12px;padding:6px 10px;background:#1a1a1a}}\
        .item.hot{{background:#24402a}}\
        .idx{{width:36px;color:#888;font-size:12px}}\
        .label{{flex:1}}\
        .tag{{padding:2px 8px;border-radius:10px;background:#2b2b2b;font-size:11px}}\
        .item button{{padding:4px 10px;border:0;border-radius:4px;background:#e5e5e5}}\
        </style></head><body>{body}</body></html>"
    );
    Document::parse(&src)
}

fn engine_9k() -> Engine {
    let mut e = Engine::with_document(bench_document(ITEMS), Box::new(MapAssets::default()));
    e.set_viewport(1000.0, 700.0, 1.0);
    e.update(0.0);
    assert_eq!(e.doc.node_count(), 9009, "bench tree shape changed; update benches + audit");
    e
}

/// Full box-tree build + Taffy layout + display-list paint from scratch.
fn bench_build_root_9k(c: &mut Criterion) {
    c.bench_function("core/build_root_9k_fresh_engine", |b| {
        b.iter_batched(
            || bench_document(ITEMS),
            |doc| {
                let mut e = Engine::with_document(doc, Box::new(MapAssets::default()));
                e.set_viewport(1000.0, 700.0, 1.0);
                e.update(0.0);
                black_box(e.display_list().items.len());
            },
            criterion::BatchSize::SmallInput,
        );
    });
}

/// Single-class mutation: incremental restyle, then the whole-tree rebuild P1 removes.
fn bench_mutation_toggle_class(c: &mut Criterion) {
    c.bench_function("core/mutation_toggle_class", |b| {
        let mut e = engine_9k();
        let mut i = 0;
        b.iter(|| {
            let id = e.doc.get_element_by_id(&format!("i{}", (i * 37) % ITEMS)).unwrap();
            e.doc.toggle_class(id, "hot", None);
            e.update(i as f64 * 0.016);
            i += 1;
            black_box(e.stats.layouts);
        });
    });
}

/// Text edit on one leaf: restyle + rebuild + reshape one paragraph.
fn bench_mutation_text_change(c: &mut Criterion) {
    c.bench_function("core/mutation_text_change", |b| {
        let mut e = engine_9k();
        let mut i = 0;
        b.iter(|| {
            let id = e.doc.get_element_by_id(&format!("i{}", (i * 53) % ITEMS)).unwrap();
            let label = e.doc.query_selector(id, ".label").unwrap().unwrap();
            e.doc.set_text_content(label, &format!("Updated {i}"));
            e.update(i as f64 * 0.016);
            i += 1;
            black_box(e.stats.layouts);
        });
    });
}

/// Viewport resize with static sheets: full restyle + relayout (P2/P3 target path).
fn bench_resize_static_sheets(c: &mut Criterion) {
    c.bench_function("core/resize_static_sheets", |b| {
        let mut e = engine_9k();
        let mut i = 0;
        b.iter(|| {
            e.set_viewport(1000.0 - (i % 2) as f32 * 40.0, 700.0, 1.0);
            e.update(i as f64 * 0.016);
            i += 1;
            black_box((e.stats.full_restyles, e.stats.layouts));
        });
    });
}

/// Display-list flatten traversal (the paint walk P5 allocation work touches).
fn bench_flatten_traversal(c: &mut Criterion) {
    let e = engine_9k();
    c.bench_function("core/flatten_display_list_traversal", |b| {
        b.iter(|| {
            let list = e.display_list();
            let mut n = 0usize;
            for item in &list.items {
                n += std::mem::size_of_val(item) & 0;
                n += 1;
            }
            black_box(n);
        });
    });
}

criterion_group!(
    benches,
    bench_build_root_9k,
    bench_mutation_toggle_class,
    bench_mutation_text_change,
    bench_resize_static_sheets,
    bench_flatten_traversal
);
criterion_main!(benches);
