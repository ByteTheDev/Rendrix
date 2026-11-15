//! D2/D3 tests: hot-reload module updates (D2) and the inspector snapshot (D3).
//!
//! `hot_reload.rs` / `inspector.rs` are new crate files that the main agent wires into
//! `lib.rs` (`pub mod hot_reload; pub mod inspector;`); until then this integration test
//! includes them by path. They address the crate through `crate::{bundle, session}`, which is
//! satisfied here by re-exporting those modules at the test-crate root.

use rendrix_runtime::{bundle, session};
use std::collections::BTreeMap;

#[path = "../src/hot_reload.rs"]
mod hot_reload;
#[path = "../src/inspector.rs"]
mod inspector;

use rendrix_runtime::bundle::AppBundle;
use rendrix_runtime::session::{KeyInput, MemoryClipboard, Mods, Session};

fn bundle_with(html: &str, modules: &[(&str, &str)]) -> AppBundle {
    let mut b = AppBundle::from_json(
        r#"{"format":"rendrix-app/1","name":"it","html":"","width":400,"height":300}"#,
    )
    .unwrap();
    b.html = html.to_string();
    for (k, v) in modules {
        b.modules.insert(k.to_string(), v.to_string());
    }
    b
}

fn new_session(b: &AppBundle) -> Session {
    let mut s = Session::new(b, Box::new(MemoryClipboard::default()), (400.0, 300.0, 1.0))
        .expect("session");
    s.js.host.borrow_mut().echo_console = false;
    s.frame();
    s
}

fn eval(s: &mut Session, expr: &str) -> String {
    match s.js.eval(&format!(
        "(()=>{{ const v = ({expr}); return typeof v === 'string' ? v : JSON.stringify(v); }})()"
    )) {
        Ok(rendrix_js::Val::Str(v)) => v,
        other => panic!("eval({expr}) -> {other:?}"),
    }
}

fn text_of(s: &mut Session, sel: &str) -> String {
    s.frame();
    let e = s.engine.borrow();
    let id = e
        .doc
        .query_selector(e.doc.root(), sel)
        .unwrap()
        .unwrap_or_else(|| panic!("no {sel}"));
    e.doc.text_content(id)
}

const PAGE: &str = "<html><head><style>body{margin:0} #out{width:200px;height:40px}</style></head><body><div id=out></div><input id=name><script type=module src=main.js></script></body></html>";

/// What `buildApp` emits for an imported `theme.css`: a JS module that (re)installs a `<style>`.
fn theme_module(color: &str) -> String {
    format!(
        "const prev = document.getElementById('hot-theme'); if (prev) prev.remove(); \
         const st = document.createElement('style'); st.id = 'hot-theme'; \
         st.textContent = '#out{{color:{color}}}'; document.head.appendChild(st);"
    )
}

#[test]
fn hot_reload_css_and_js_edit_without_restart() {
    let js_v1 = "import './theme.js'; import { label } from './lib.js'; const out = document.getElementById('out'); if (!out.textContent) out.textContent = label;";
    let b = bundle_with(
        PAGE,
        &[
            ("main.js", js_v1),
            ("theme.js", &theme_module("rgb(0, 0, 0)")),
            ("lib.js", "export const label = 'v1';"),
        ],
    );
    let mut s = new_session(&b);
    // Script cooperates: it only initialises once, so later state is preserved by the reload.
    assert_eq!(text_of(&mut s, "#out"), "v1");
    assert_eq!(
        eval(
            &mut s,
            "getComputedStyle(document.getElementById('out')).color"
        ),
        "rgb(0, 0, 0)"
    );
    // Focus the input and type: focus + value must survive the reload (same Engine/DOM).
    let (x, y) = {
        s.frame();
        let id = s.query("#name").expect("input");
        s.center_of(id).expect("box")
    };
    s.pointer_down(x, y, 0);
    s.pointer_up(x, y, 0);
    assert_eq!(eval(&mut s, "document.activeElement.id"), "name");
    for ch in "keep".chars() {
        let k = KeyInput {
            key: ch.to_string(),
            code: String::new(),
            text: Some(ch.to_string()),
            repeat: false,
            mods: Mods::default(),
        };
        s.key_down(k.clone());
        s.key_up(k);
    }
    assert_eq!(
        eval(&mut s, "document.getElementById('name').value"),
        "keep"
    );

    // The "edited project": new theme colour, new module code. The helper gets the FULL set.
    let js_v2 = "import './theme.js'; import { label } from './lib.js'; const out = document.getElementById('out'); out.dataset.hot = 'yes'; if (!out.textContent) out.textContent = label;";
    let mut changed = BTreeMap::new();
    changed.insert("main.js".into(), js_v2.into());
    changed.insert("theme.js".into(), theme_module("rgb(0, 128, 0)"));
    changed.insert("lib.js".into(), "export const label = 'v2';".into());
    let ran = hot_reload::apply_module_update(&mut s, changed).expect("hot update applies");
    assert!(
        ran.iter().any(|r| r.contains("main.js")),
        "entry re-ran: {ran:?}"
    );

    // Same process, same session: the new theme applied, the new code ran, old state survived.
    assert_eq!(
        eval(
            &mut s,
            "getComputedStyle(document.getElementById('out')).color"
        ),
        "rgb(0, 128, 0)",
        "updated CSS applied"
    );
    assert_eq!(
        eval(&mut s, "document.getElementById('out').dataset.hot"),
        "yes",
        "updated module code executed"
    );
    assert_eq!(
        text_of(&mut s, "#out"),
        "v1",
        "cooperative script state preserved"
    );
    assert_eq!(
        eval(&mut s, "document.activeElement.id"),
        "name",
        "focus preserved across reload"
    );
    assert_eq!(
        eval(&mut s, "document.getElementById('name').value"),
        "keep",
        "input value preserved across reload"
    );
}

#[test]
fn hot_reload_failed_update_keeps_last_good() {
    let js_v1 = "globalThis.n = (globalThis.n || 0); const out = document.getElementById('out'); out.textContent = 'v1:' + globalThis.n; out.addEventListener('click', () => { globalThis.n += 1; out.textContent = 'v1:' + globalThis.n; });";
    let b = bundle_with(PAGE, &[("main.js", js_v1)]);
    let mut s = new_session(&b);
    assert_eq!(text_of(&mut s, "#out"), "v1:0");

    // A broken edit: syntax error at the top, before any DOM mutation.
    let mut changed = BTreeMap::new();
    changed.insert("main.js".into(), "const broken = {{{;".into());
    let err =
        hot_reload::apply_module_update(&mut s, changed).expect_err("broken update must fail");
    assert!(err.contains("main.js"), "error names the module: {err}");

    // Last-good is intact: old text and the old click handler still work in the same session.
    assert_eq!(
        text_of(&mut s, "#out"),
        "v1:0",
        "DOM untouched by the failed update"
    );
    s.frame();
    let (x, y) = s.center_of(s.query("#out").unwrap()).unwrap();
    s.pointer_down(x, y, 0);
    s.pointer_up(x, y, 0);
    assert_eq!(
        text_of(&mut s, "#out"),
        "v1:1",
        "previous scripts keep working after a rejected update"
    );

    // A missing import also fails closed.
    let mut changed = BTreeMap::new();
    changed.insert("main.js".into(), "import './does-not-exist.js';".into());
    let err = hot_reload::apply_module_update(&mut s, changed)
        .expect_err("unresolvable update must fail");
    assert!(
        err.contains("does-not-exist"),
        "error names the missing module: {err}"
    );
    assert_eq!(text_of(&mut s, "#out"), "v1:1");
}

#[test]
fn inspector_snapshot_shape() {
    let js = "console.error('boom-inspect'); document.getElementById('out').textContent = 'hi';";
    let b = bundle_with(PAGE, &[("main.js", js)]);
    let mut s = new_session(&b);
    assert_eq!(text_of(&mut s, "#out"), "hi");
    let snap = inspector::snapshot(&s);
    assert_eq!(snap["app"], "it");
    assert_eq!(snap["viewport"]["width"], 400.0);
    assert!(snap["stats"]["frames"].as_u64().unwrap() >= 1, "{snap}");
    for key in [
        "restyles",
        "layouts",
        "last_style_micros",
        "last_layout_micros",
        "shape_count",
        "cache_hits",
        "cache_len",
    ] {
        assert!(
            snap["stats"][key].is_number(),
            "stats.{key} present: {snap}"
        );
    }
    assert!(
        snap["display_items"].as_u64().unwrap() > 0,
        "display list non-empty"
    );
    assert!(
        snap["node_count"].as_u64().unwrap() >= 5,
        "html/head/body/div/input/script nodes"
    );
    assert!(!snap["nodes"].as_array().unwrap().is_empty());
    let out = snap["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|n| n["id"] == "out")
        .expect("node table has #out");
    assert_eq!(out["tag"], "div");
    assert!(out["rect"].is_array(), "#out has a layout rect");
    assert_eq!(out["display"], "block");
    assert_eq!(snap["js"]["engine"], "quickjs");
    assert!(snap["js"]["memory_bytes"].as_u64().unwrap() > 0);
    let errors: Vec<&str> = snap["recent_errors"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|e| e["message"].as_str())
        .collect();
    assert!(
        errors.iter().any(|m| m.contains("boom-inspect")),
        "console errors surfaced: {errors:?}"
    );
}
