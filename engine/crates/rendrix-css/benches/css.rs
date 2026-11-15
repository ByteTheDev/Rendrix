//! M0 micro-benches for the CSS engine: parse, RuleSet build, cascade.
//!
//! Dev-only harness (`criterion`, never shipped). Timing is informational in CI;
//! use the medians to validate P2/P3/P5 changes against the PERFORMANCE_AUDIT baseline.

use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion};
use rendrix_css::{
    compute_style, parse_declarations, ElementState, MediaContext, Origin, RuleSet, SelectorElement,
    StyleContext, Stylesheet,
};

/// Minimal element so the cascade can be benched without pulling in rendrix-dom.
#[derive(Clone)]
struct FakeEl {
    tag: &'static str,
    id: Option<&'static str>,
    classes: Vec<&'static str>,
    class_attr: String,
    parent: Option<Box<FakeEl>>,
}

impl FakeEl {
    fn new(tag: &'static str, id: Option<&'static str>, classes: Vec<&'static str>, parent: Option<Box<FakeEl>>) -> Self {
        let class_attr = classes.join(" ");
        FakeEl { tag, id, classes, class_attr, parent }
    }
}

impl SelectorElement for FakeEl {
    fn tag(&self) -> &str {
        self.tag
    }
    fn id(&self) -> Option<&str> {
        self.id
    }
    fn has_class(&self, class: &str) -> bool {
        self.classes.iter().any(|c| *c == class)
    }
    fn attr(&self, name: &str) -> Option<&str> {
        if name == "class" { Some(&self.class_attr) } else { None }
    }
    fn parent_element(&self) -> Option<Self> {
        self.parent.clone().map(|b| *b)
    }
    fn prev_sibling_element(&self) -> Option<Self> {
        None
    }
    fn next_sibling_element(&self) -> Option<Self> {
        None
    }
    fn has_state(&self, _state: ElementState) -> bool {
        false
    }
    fn is_root(&self) -> bool {
        self.parent.is_none()
    }
    fn is_empty(&self) -> bool {
        true
    }
}

/// Stylesheet shaped like the `bench 1000` page: enough selectors to be realistic.
fn bench_css() -> String {
    let mut s = String::from(
        "body{margin:0;background:#101010;color:#ddd;font:14px sans-serif}\n\
         #list{padding:8px;display:flex;flex-direction:column;gap:4px}\n\
         .item{display:flex;align-items:center;gap:12px;padding:6px 10px;}\n\
         .item.hot{background:#24402a}\n\
         .idx{width:36px;color:#888;font-size:12px}\n\
         .label{flex:1}\n\
         .tag{padding:2px 8px;border-radius:10px;background:#2b2b2b;font-size:11px}\n\
         .item button{padding:4px 10px;border:0;border-radius:4px;background:#e5e5e5}\n\
         @media (max-width: 500px){.item{flex-direction:column}}\n",
    );
    for i in 0..200 {
        s.push_str(&format!(".gen{i}{{color:rgb({i},{i},{i});margin:{i}px}}\n"));
    }
    s
}

fn ruleset() -> (RuleSet, MediaContext) {
    let sheet = Stylesheet::parse(&bench_css());
    let media = MediaContext::default();
    let mut rules = RuleSet::new();
    rules.add_stylesheet(&sheet, Origin::Author, &media);
    (rules, media)
}

fn target() -> FakeEl {
    FakeEl::new(
        "div",
        Some("i42"),
        vec!["item", "hot", "label"],
        Some(Box::new(FakeEl::new("div", Some("list"), vec![], None))),
    )
}

fn bench_parse(c: &mut Criterion) {
    let src = bench_css();
    c.bench_function("css/parse_bench_stylesheet", |b| {
        b.iter(|| black_box(Stylesheet::parse(black_box(&src))));
    });
}

fn bench_add_stylesheet(c: &mut Criterion) {
    let sheet = Stylesheet::parse(&bench_css());
    let media = MediaContext::default();
    c.bench_function("css/ruleset_add_stylesheet", |b| {
        b.iter(|| {
            let mut rules = RuleSet::new();
            rules.add_stylesheet(black_box(&sheet), Origin::Author, black_box(&media));
            black_box(rules.len())
        });
    });
}

fn bench_compute_single(c: &mut Criterion) {
    let (rules, _) = ruleset();
    let el = target();
    let inline = parse_declarations("color: red; width: 10px");
    let parent = rendrix_css::ComputedStyle::default();
    let sc = StyleContext::default();
    c.bench_function("css/compute_style_single", |b| {
        b.iter(|| {
            black_box(compute_style(
                black_box(&el),
                black_box(&rules),
                black_box(&inline),
                Some(black_box(&parent)),
                black_box(&sc),
                None,
            ))
        });
    });
}

/// ~9k elements through the same RuleSet (the `bench 1000` tree shape).
fn bench_compute_9k(c: &mut Criterion) {
    let (rules, _) = ruleset();
    let sc = StyleContext::default();
    let parent = rendrix_css::ComputedStyle::default();
    let els: Vec<FakeEl> = (0..9000)
        .map(|i| {
            FakeEl::new(
                if i % 7 == 0 { "button" } else { "div" },
                None,
                vec![if i % 3 == 0 { "item" } else { "label" }],
                None,
            )
        })
        .collect();
    let inline = Vec::new();
    c.bench_function("css/compute_style_9k_tree", |b| {
        b.iter(|| {
            let mut n = 0;
            for el in &els {
                n += compute_style(el, &rules, &inline, Some(&parent), &sc, None).font_size as usize;
            }
            black_box(n);
        });
    });
}

fn bench_selector_matching(c: &mut Criterion) {
    let mut g = c.benchmark_group("css/compute_style_ruleset_size");
    for &extra in &[0usize, 500, 1000] {
        let mut rules = ruleset().0;
        let big = Stylesheet::parse(&(0..extra).map(|i| format!(".pad{i}{{padding:{i}px}}")).collect::<String>());
        rules.add_stylesheet(&big, Origin::Author, &MediaContext::default());
        let el = target();
        let sc = StyleContext::default();
        let parent = rendrix_css::ComputedStyle::default();
        let inline = Vec::new();
        g.bench_with_input(BenchmarkId::from_parameter(extra), &extra, |b, _| {
            b.iter(|| black_box(compute_style(&el, &rules, &inline, Some(&parent), &sc, None)));
        });
    }
    g.finish();
}

criterion_group!(benches, bench_parse, bench_add_stylesheet, bench_compute_single, bench_compute_9k, bench_selector_matching);
criterion_main!(benches);
