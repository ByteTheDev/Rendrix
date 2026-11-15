//! M0 micro-benches for Taffy layout: fixed vs varying widths.
//!
//! Dev-only harness (`criterion`, never shipped). Taffy keeps its tree between frames,
//! so the interesting split is cache-friendly (same widths) vs cache-hostile (new widths).

use criterion::{black_box, criterion_group, criterion_main, Criterion};
use rendrix_css::{apply_declaration, ComputedStyle, StyleContext};
use rendrix_layout::{InlineContent, LayoutEngine, LayoutKind, LayoutNode, TaffyEngine, TextMeasurer};
use std::sync::Arc;

struct FixedText;
impl TextMeasurer for FixedText {
    fn measure(&mut self, c: &InlineContent, max_width: Option<f32>) -> (f32, f32) {
        let text: String = c.runs.iter().map(|r| r.text.as_str()).collect();
        let w = text.chars().count() as f32 * 10.0;
        match max_width {
            Some(m) if m > 0.0 && w > m => {
                let per_line = (m / 10.0).floor().max(1.0);
                let lines = (text.chars().count() as f32 / per_line).ceil();
                (per_line * 10.0, lines * 20.0)
            }
            _ => (w, 20.0),
        }
    }
}

fn style(decls: &[(&str, &str)]) -> Arc<ComputedStyle> {
    let parent = ComputedStyle::default();
    let mut s = ComputedStyle::inherit_from(&parent);
    s.display = rendrix_css::Display::Block;
    let sc = StyleContext::default();
    for (k, v) in decls {
        apply_declaration(&mut s, &parent, k, v, &sc);
    }
    Arc::new(s)
}

fn node(id: u64, decls: &[(&str, &str)], children: Vec<LayoutNode>) -> LayoutNode {
    LayoutNode { id, style: style(decls), kind: LayoutKind::Box, children }
}

/// 9k-box column shaped like the `bench 1000` list.
fn bench_tree(width: &str) -> LayoutNode {
    let kids: Vec<LayoutNode> = (0..1000)
        .map(|i| {
            node(
                100 + i as u64,
                &[("display", "flex"), ("padding", "6px 10px")],
                vec![
                    node(100000 + i as u64, &[("width", "36px"), ("height", "12px")], vec![]),
                    node(200000 + i as u64, &[("flex", "1"), ("height", "16px")], vec![]),
                    node(300000 + i as u64, &[("width", "60px"), ("height", "14px")], vec![]),
                ],
            )
        })
        .collect();
    node(1, &[("display", "flex"), ("flex-direction", "column"), ("width", width)], kids)
}

fn bench_layout_fixed_width(c: &mut Criterion) {
    c.bench_function("taffy/layout_9k_fixed_width", |b| {
        let root = bench_tree("1000px");
        let mut e = TaffyEngine::new();
        let mut m = FixedText;
        e.layout(&root, (1000.0, 700.0), &mut m);
        assert_eq!((e.last_new_nodes > 0, e.last_restyled_nodes), (true, 0));
        b.iter(|| black_box(e.layout(black_box(&root), (1000.0, 700.0), &mut m)));
    });
}

fn bench_layout_varying_width(c: &mut Criterion) {
    c.bench_function("taffy/layout_9k_varying_width", |b| {
        let mut e = TaffyEngine::new();
        let mut m = FixedText;
        // Leaked once: 41 distinct widths cycle to defeat the layout cache without
        // leaking per iteration.
        let widths: Vec<&str> = (0..41).map(|k| Box::leak(format!("{}px", 960 + k).into_boxed_str()) as &str).collect();
        let mut i = 0;
        b.iter(|| {
            // A new width per iteration defeats both the Taffy cache and any
            // style-Arc sharing: the resize-storm path P3/P4 must handle.
            let root = bench_tree(widths[i % widths.len()]);
            let r = e.layout(&root, (1000.0, 700.0), &mut m);
            i += 1;
            black_box(r.boxes.len());
        });
    });
}

criterion_group!(benches, bench_layout_fixed_width, bench_layout_varying_width);
criterion_main!(benches);
