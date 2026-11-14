//! M0 micro-benches for text shaping: paragraph cache hit vs miss.
//!
//! Dev-only harness (`criterion`, never shipped). The `shape_count` delta per bench
//! is the P4 acceptance signal: hits must not reshape, misses must.

use criterion::{black_box, criterion_group, criterion_main, Criterion};
use rendrix_css::ComputedStyle;
use rendrix_gfx::TextSystem;
use rendrix_layout::{InlineContent, InlineRun, TextMeasurer};
use std::sync::Arc;

fn content(text: &str, size: f32) -> InlineContent {
    let mut s = ComputedStyle::default();
    s.font_size = size;
    let s = Arc::new(s);
    InlineContent { runs: vec![InlineRun { text: text.into(), style: s.clone(), node: 1, is_break: false }], container: s }
}

fn bench_paragraph_hit(c: &mut Criterion) {
    let mut t = TextSystem::new();
    let para = content("Item number 42 with some descriptive text", 14.0);
    t.paragraph(&para, Some(400.0));
    let shaped = t.shape_count;
    c.bench_function("text/paragraph_cache_hit", |b| {
        b.iter(|| black_box(t.paragraph(black_box(&para), Some(400.0))));
    });
    assert_eq!(t.shape_count, shaped, "hits must not reshape");
}

fn bench_paragraph_miss(c: &mut Criterion) {
    c.bench_function("text/paragraph_cache_miss", |b| {
        let mut t = TextSystem::new();
        let mut i = 0;
        b.iter(|| {
            // Distinct text per iteration forces a fresh shape.
            let para = content(&format!("Unique line {i} with enough words to shape"), 14.0);
            let p = t.paragraph(&para, Some(400.0));
            i += 1;
            black_box(p.width);
        });
    });
}

fn bench_measure_wrap(c: &mut Criterion) {
    let mut t = TextSystem::new();
    let para = content("hello world this wraps over several lines of text for measurement", 16.0);
    c.bench_function("text/measure_wrapped", |b| {
        b.iter(|| black_box(t.measure(black_box(&para), Some(120.0))));
    });
}

criterion_group!(benches, bench_paragraph_hit, bench_paragraph_miss, bench_measure_wrap);
criterion_main!(benches);
