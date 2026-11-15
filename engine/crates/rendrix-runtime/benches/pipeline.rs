//! M0 pipeline benches: scripted `Session` frames (no GPU) + allocation counters.
//!
//! Dev-only harness (`criterion`, never shipped). Timing medians here must track the
//! `bench 1000` CPU columns in PERFORMANCE_AUDIT.md; the `allocated_bytes` black-boxed
//! per scenario is the P5 acceptance signal (same frames, fewer bytes).

use criterion::{black_box, criterion_group, criterion_main, Criterion};
use rendrix_runtime::bundle::AppBundle;
use rendrix_runtime::session::{MemoryClipboard, Session};
use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};

/// Dev-only counting allocator: per-scenario allocated-byte deltas, no sampling.
struct CountingAlloc;

static ALLOCATED: AtomicUsize = AtomicUsize::new(0);
static FREED: AtomicUsize = AtomicUsize::new(0);

unsafe impl GlobalAlloc for CountingAlloc {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let p = unsafe { System.alloc(layout) };
        if !p.is_null() {
            ALLOCATED.fetch_add(layout.size(), Ordering::Relaxed);
        }
        p
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) };
        FREED.fetch_add(layout.size(), Ordering::Relaxed);
    }
}

#[global_allocator]
static GLOBAL: CountingAlloc = CountingAlloc;

fn alloc_delta(f: impl FnOnce()) -> (usize, usize) {
    let (a0, f0) = (ALLOCATED.load(Ordering::Relaxed), FREED.load(Ordering::Relaxed));
    f();
    (ALLOCATED.load(Ordering::Relaxed) - a0, FREED.load(Ordering::Relaxed) - f0)
}

const ITEMS: usize = 1000;

fn bundle(items: usize) -> AppBundle {
    let html = r#"<html><head><style>
      body{margin:0;background:#101010;color:#ddd;font:14px sans-serif}
      #list{padding:8px;display:flex;flex-direction:column;gap:4px}
      .item{display:flex;align-items:center;gap:12px;padding:6px 10px;background:#1a1a1a}
      .item.hot{background:#24402a}
      .idx{width:36px;color:#888;font-size:12px}
      .label{flex:1}
      .tag{padding:2px 8px;border-radius:10px;background:#2b2b2b;font-size:11px}
      .item button{padding:4px 10px;border:0;border-radius:4px;background:#e5e5e5}
    </style></head><body><div id=list></div><script type=module src=main.js></script></body></html>"#;
    let js = format!(
        "const list = document.getElementById('list'); const frag = document.createDocumentFragment();
         for (let i = 0; i < {items}; i++) {{ const d = document.createElement('div'); d.className = 'item'; d.id = 'i' + i;
           d.innerHTML = '<span class=idx>' + i + '</span><span class=label>Item number ' + i + ' with some descriptive text</span><span class=tag>tag ' + (i % 7) + '</span><button>Open</button>'; frag.appendChild(d); }}
         list.appendChild(frag);"
    );
    let mut b = AppBundle::from_json(r#"{"format":"rendrix-app/1","name":"bench","html":"","width":1000,"height":700}"#).unwrap();
    b.html = html.to_string();
    b.modules.insert("main.js".into(), js);
    b
}

fn session() -> Session {
    let mut s = Session::new(&bundle(ITEMS), Box::new(MemoryClipboard::default()), (1000.0, 700.0, 1.0)).expect("session");
    s.js.host.borrow_mut().echo_console = false;
    s.frame();
    assert_eq!(s.engine.borrow().doc.node_count(), 9009, "bench tree shape changed; update benches + audit");
    s
}

macro_rules! frame_bench {
    ($c:expr, $name:expr, $setup:expr, $op:expr) => {{
        $c.bench_function($name, |b| {
            let mut s = $setup();
            let mut i: usize = 0;
            // Warm up caches (paragraph, glyph) before measuring.
            for k in 0..3 {
                $op(&mut s, k);
                s.frame();
            }
            let shapes0 = s.engine.borrow().text_system().shape_count;
            let stats0 = s.engine.borrow().stats.clone();
            b.iter(|| {
                $op(&mut s, i + 3);
                let (alloc, _freed) = alloc_delta(|| {
                    s.frame();
                });
                i += 1;
                black_box(alloc);
            });
            let shapes1 = s.engine.borrow().text_system().shape_count;
            let stats1 = s.engine.borrow().stats.clone();
            eprintln!(
                "[alloc] {}: shapes +{}, restyles +{}, layouts +{}, frames +{}",
                $name,
                shapes1 - shapes0,
                stats1.restyles - stats0.restyles,
                stats1.layouts - stats0.layouts,
                stats1.frames - stats0.frames,
            );
        });
    }};
}

fn pipeline(c: &mut Criterion) {
    frame_bench!(c, "pipeline/idle_frame", | | session(), |_: &mut Session, _: usize| {});
    frame_bench!(c, "pipeline/toggle_class", | | session(), |s: &mut Session, i: usize| {
        let id = s.engine.borrow().doc.get_element_by_id(&format!("i{}", (i * 37) % ITEMS)).unwrap();
        s.engine.borrow_mut().doc.toggle_class(id, "hot", None);
    });
    frame_bench!(c, "pipeline/text_change", | | session(), |s: &mut Session, i: usize| {
        let mut e = s.engine.borrow_mut();
        let id = e.doc.get_element_by_id(&format!("i{}", (i * 53) % ITEMS)).unwrap();
        let label = e.doc.query_selector(id, ".label").unwrap().unwrap();
        e.doc.set_text_content(label, &format!("Updated {i}"));
    });
    frame_bench!(c, "pipeline/hover", | | session(), |s: &mut Session, i: usize| {
        s.pointer_move(500.0, 30.0 + (i % 12) as f32 * 34.0);
    });
    frame_bench!(c, "pipeline/scroll", | | session(), |s: &mut Session, i: usize| {
        s.wheel(500.0, 300.0, 0.0, if i % 2 == 0 { 48.0 } else { -48.0 });
    });
    frame_bench!(c, "pipeline/add_remove_10", | | session(), |s: &mut Session, i: usize| {
        let mut e = s.engine.borrow_mut();
        let list = e.doc.get_element_by_id("list").unwrap();
        for k in 0..10 {
            let d = e.doc.create_element("div");
            e.doc.set_attr(d, "class", "item");
            e.doc.set_inner_html(d, &format!("<span class=idx>{k}</span><span class=label>added {i}</span>"));
            e.doc.append_child(list, d).unwrap();
        }
        let kids: Vec<_> = e.doc.children(list).collect();
        for k in kids.iter().rev().take(10) {
            let _ = e.doc.remove_child(list, *k);
        }
    });
    frame_bench!(c, "pipeline/resize", | | session(), |s: &mut Session, i: usize| {
        s.resize(1000.0 - (i % 2) as f32 * 40.0, 700.0, 1.0);
    });
    frame_bench!(c, "pipeline/js_toggle_class", | | session(), |s: &mut Session, i: usize| {
        let _ = s.js.eval(&format!("document.getElementById('i{}').classList.toggle('hot')", (i * 11) % ITEMS));
    });
    // Display-list flatten traversal (paint walk, no GPU submit).
    c.bench_function("pipeline/flatten_traversal", |b| {
        let s = session();
        b.iter(|| {
            let (alloc, _) = alloc_delta(|| {
                let e = s.engine.borrow();
                let list = e.display_list();
                black_box(list.items.len());
            });
            black_box(alloc);
        });
    });
}

criterion_group!(benches, pipeline);
criterion_main!(benches);
