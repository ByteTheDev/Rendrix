use crate::*;
use rendrix_core::{Engine, MapAssets};
use rendrix_dom::Document;
use std::cell::RefCell;
use std::rc::Rc;

fn rt_with(html: &str, assets: MapAssets) -> JsRuntime {
    let doc = Document::parse(html);
    let mut engine = Engine::with_document(doc, Box::new(assets));
    engine.set_viewport(400.0, 300.0, 1.0);
    let engine = Rc::new(RefCell::new(engine));
    let rt = JsRuntime::new(engine, "rendrix-test", Permissions::allow_all()).expect("runtime");
    rt.host.borrow_mut().echo_console = false;
    rt.host.borrow_mut().disable_storage_persistence();
    rt
}

fn rt(html: &str) -> JsRuntime {
    rt_with(html, MapAssets::default())
}

fn run(rt: &mut JsRuntime, src: &str) {
    if let Err(e) = rt.run_script("test", src) {
        panic!("script failed: {e}");
    }
}

fn s(rt: &mut JsRuntime, expr: &str) -> String {
    match rt.eval(&format!("(()=>{{ const v = ({expr}); return typeof v === 'string' ? v : JSON.stringify(v); }})()")) {
        Ok(Val::Str(s)) => s,
        Ok(other) => format!("{other:?}"),
        Err(e) => panic!("eval failed: {e}"),
    }
}

fn pump(rt: &mut JsRuntime, ms: u64) {
    let end = std::time::Instant::now() + std::time::Duration::from_millis(ms);
    while std::time::Instant::now() < end {
        rt.tick();
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
    rt.tick();
}

fn logs(rt: &mut JsRuntime) -> Vec<String> {
    rt.host.borrow_mut().take_console().into_iter().map(|(l, m)| format!("{l}:{m}")).collect()
}

#[test]
fn engine_runs_modern_javascript() {
    let mut r = rt("");
    assert_eq!(s(&mut r, "[1,2,3].map(x => x ** 2).join(',')"), "1,4,9");
    run(&mut r, "class A { #p = 41; static s = 1; get v() { return this.#p + 1; } } globalThis.out = [new A().v, A.s, [...'ab'], `t${1+1}`, 1_000, 10n ** 3n === 1000n, a?.b ?? 'dflt'].toString(); var a;");
    assert_eq!(s(&mut r, "globalThis.out"), "42,1,a,b,t2,1000,true,dflt");
    assert_eq!(r.engine_name(), "quickjs");
}

#[test]
fn promises_microtasks_and_async_await_run_in_order() {
    let mut r = rt("");
    run(
        &mut r,
        "globalThis.order = []; const log = x => order.push(x);
         (async () => { log('a1'); await null; log('a2'); await new Promise(r => setTimeout(r, 5)); log('a3'); })();
         Promise.resolve().then(() => log('p1')); queueMicrotask(() => log('m1')); setTimeout(() => log('t0'), 0); log('sync');",
    );
    assert_eq!(s(&mut r, "order"), r#"["a1","sync","a2","p1","m1"]"#, "microtasks ran after the script, before timers");
    pump(&mut r, 40);
    assert_eq!(s(&mut r, "order"), r#"["a1","sync","a2","p1","m1","t0","a3"]"#);
}

#[test]
fn timers_intervals_and_clearing() {
    let mut r = rt("");
    run(&mut r, "globalThis.n = 0; globalThis.id = setInterval(() => { n++; if (n === 3) clearInterval(id); }, 5); const t = setTimeout(() => { n = 100; }, 5); clearTimeout(t);");
    pump(&mut r, 80);
    assert_eq!(s(&mut r, "n"), "3");
    assert!(r.tick_info().next_timer_ms.is_none(), "no timers left");
}

#[test]
fn request_animation_frame_runs_on_frame_not_on_tick() {
    let mut r = rt("");
    run(&mut r, "globalThis.frames = []; const loop = t => { frames.push(typeof t); if (frames.length < 3) requestAnimationFrame(loop); }; requestAnimationFrame(loop);");
    assert!(r.tick_info().wants_frame);
    pump(&mut r, 10);
    assert_eq!(s(&mut r, "frames.length"), "0", "rAF waits for the frame driver");
    r.run_animation_frame(16.0);
    assert!(r.tick_info().wants_frame, "callback re-requested a frame");
    r.run_animation_frame(32.0);
    r.run_animation_frame(48.0);
    assert_eq!(s(&mut r, "frames.join()"), "number,number,number");
    assert!(!r.tick_info().wants_frame);
}

#[test]
fn dom_creation_manipulation_and_queries() {
    let mut r = rt("<div id=app><ul class='list'><li class='a'>1</li><li>2</li></ul></div>");
    run(
        &mut r,
        "const ul = document.querySelector('ul'); const li = document.createElement('li'); li.textContent = 'three'; li.classList.add('x', 'y'); li.dataset.fooBar = '7'; ul.appendChild(li);
         globalThis.res = { n: ul.children.length, last: ul.lastElementChild.textContent, cls: li.className, data: li.getAttribute('data-foo-bar'), ds: li.dataset.fooBar,
           q: document.querySelectorAll('li.x').length, closest: li.closest('#app').id, matches: li.matches('ul > li:last-child'), same: ul.lastChild === li && document.querySelector('.y') === li,
           html: document.getElementById('app').innerHTML.length > 20, tag: li.tagName, name: li.nodeName, parent: li.parentNode === ul, doc: li.ownerDocument === document };",
    );
    assert_eq!(
        s(&mut r, "res"),
        r#"{"n":3,"last":"three","cls":"x y","data":"7","ds":"7","q":1,"closest":"app","matches":true,"same":true,"html":true,"tag":"LI","name":"LI","parent":true,"doc":true}"#
    );
    run(&mut r, "ul.insertBefore(li, ul.firstElementChild); li.remove(); ul.innerHTML = '<b>x</b><i>y</i>'; globalThis.after = [ul.children.length, ul.textContent, li.isConnected];");
    assert_eq!(s(&mut r, "after"), r#"[2,"xy",false]"#);
}

#[test]
fn dom_errors_are_domexceptions() {
    let mut r = rt("<div id=a><span id=b></span></div>");
    run(
        &mut r,
        "const a = document.getElementById('a'), b = document.getElementById('b'); globalThis.errs = [];
         try { b.appendChild(a); } catch (e) { errs.push([e instanceof DOMException, e.name]); }
         try { a.removeChild(document.body); } catch (e) { errs.push(e.name); }
         try { document.querySelector('div >'); } catch (e) { errs.push(e.name); }",
    );
    assert_eq!(s(&mut r, "errs"), r#"[[true,"HierarchyRequestError"],"NotFoundError","SyntaxError"]"#);
}

#[test]
fn inline_style_proxy_reads_and_writes_the_style_attribute() {
    let mut r = rt("<div id=d style='color: red'></div>");
    run(&mut r, "const d = document.getElementById('d'); d.style.backgroundColor = 'blue'; d.style.setProperty('--x', '1px'); d.style.marginTop = '4px'; globalThis.out = [d.getAttribute('style'), d.style.color, d.style.getPropertyValue('--x'), d.style.length]; d.style.color = ''; globalThis.out2 = d.style.cssText;");
    assert_eq!(s(&mut r, "out"), r#"["color: red; background-color: blue; --x: 1px; margin-top: 4px;","red","1px",4]"#);
    assert_eq!(s(&mut r, "out2"), "background-color: blue; --x: 1px; margin-top: 4px;");
}

#[test]
fn computed_style_and_geometry_reflect_layout() {
    let mut r = rt("<style>body{margin:0} #box{width:120px;height:40px;margin:10px;padding:5px;color:rgb(1,2,3);display:flex}</style><div id=box></div>");
    run(
        &mut r,
        "const b = document.getElementById('box'); const cs = getComputedStyle(b); const rc = b.getBoundingClientRect();
         globalThis.g = { w: cs.width, c: cs.color, d: cs.display, rect: [rc.x, rc.y, rc.width, rc.height], ow: b.offsetWidth, oh: b.offsetHeight, mt: cs.marginTop, via: cs.getPropertyValue('padding-left') };",
    );
    assert_eq!(s(&mut r, "g"), r#"{"w":"120px","c":"rgb(1, 2, 3)","d":"flex","rect":[10,10,130,50],"ow":130,"oh":50,"mt":"10px","via":"5px"}"#);
    run(&mut r, "document.getElementById('box').style.width = '200px'; globalThis.w2 = document.getElementById('box').offsetWidth;");
    assert_eq!(s(&mut r, "w2"), "210", "layout is recomputed synchronously after a DOM change");
}

#[test]
fn events_capture_target_bubble_order_and_control() {
    let mut r = rt("<div id=outer><div id=inner><button id=btn>x</button></div></div>");
    run(
        &mut r,
        "globalThis.log = []; const $ = id => document.getElementById(id);
         const add = (el, name, capture, extra) => el.addEventListener('click', e => { log.push(name + ':' + e.eventPhase); extra && extra(e); }, capture);
         add(window, 'win-cap', true); add(document, 'doc-cap', true); add($('outer'), 'outer-cap', true); add($('inner'), 'inner-cap', true);
         add($('btn'), 'btn-cap', true); add($('btn'), 'btn-bub', false);
         add($('inner'), 'inner-bub', false); add($('outer'), 'outer-bub', false); add(document, 'doc-bub', false); add(window, 'win-bub', false);
         $('btn').click();",
    );
    assert_eq!(
        s(&mut r, "log.join(' ')"),
        "win-cap:1 doc-cap:1 outer-cap:1 inner-cap:1 btn-cap:2 btn-bub:2 inner-bub:3 outer-bub:3 doc-bub:3 win-bub:3"
    );
    run(
        &mut r,
        "log.length = 0; const stop = e => e.stopPropagation(); $('inner').addEventListener('click', stop); $('btn').click(); globalThis.a = log.join(' ');
         $('inner').removeEventListener('click', stop); log.length = 0; let n = 0; $('btn').addEventListener('click', () => n++, { once: true }); $('btn').click(); $('btn').click(); globalThis.b = n;
         const ev = new CustomEvent('ping', { bubbles: true, cancelable: true, detail: { k: 1 } }); globalThis.d = []; document.addEventListener('ping', e => { d.push(e.detail.k, e.target.id); e.preventDefault(); });
         globalThis.ret = $('btn').dispatchEvent(ev); globalThis.prevented = ev.defaultPrevented;",
    );
    assert_eq!(s(&mut r, "a"), "win-cap:1 doc-cap:1 outer-cap:1 inner-cap:1 btn-cap:2 btn-bub:2 inner-bub:3", "stopPropagation at inner halts the bubble");
    assert_eq!(s(&mut r, "b"), "1", "once listener fires a single time");
    assert_eq!(s(&mut r, "[d, ret, prevented]"), r#"[[1,"btn"],false,true]"#);
}

#[test]
fn handler_properties_stop_immediate_and_listener_errors() {
    let mut r = rt("<button id=b>x</button>");
    run(
        &mut r,
        "const b = document.getElementById('b'); globalThis.log = [];
         b.onclick = () => log.push('prop'); b.addEventListener('click', e => { log.push('l1'); e.stopImmediatePropagation(); }); b.addEventListener('click', () => log.push('l2'));
         b.click(); b.onclick = null; log.push('|');
         b.addEventListener('mousedown', () => { throw new Error('boom'); }); b.addEventListener('mousedown', () => log.push('after-throw'));
         globalThis.winErr = []; addEventListener('error', e => winErr.push(e.message)); b.dispatchEvent(new MouseEvent('mousedown', { bubbles: true }));",
    );
    assert_eq!(s(&mut r, "log.join(',')"), "prop,l1,|,after-throw", "onclick ran first (registered first), stopImmediate skipped l2, a throwing listener does not stop others");
    assert_eq!(s(&mut r, "winErr"), r#"["boom"]"#, "uncaught listener errors surface as window 'error' events");
    assert!(logs(&mut r).iter().any(|l| l.starts_with("error:Uncaught") && l.contains("boom")));
}

#[test]
fn platform_dispatch_reaches_listeners_with_event_data() {
    let mut r = rt("<style>body{margin:0}#t{width:50px;height:50px}</style><div id=t></div>");
    run(&mut r, "globalThis.got = []; const t = document.getElementById('t'); t.addEventListener('pointerdown', e => got.push([e.type, e.clientX, e.clientY, e.button, e.isTrusted, e instanceof MouseEvent]));\
        document.addEventListener('keydown', e => { got.push([e.key, e.code, e.ctrlKey, e.target === document.body || e.target === window]); if (e.key === 'x') e.preventDefault(); });");
    let t = r.engine.borrow().doc.get_element_by_id("t").unwrap();
    r.dispatch(
        "pointerdown",
        Some(t),
        &serde_json::json!({"clientX": 12.5, "clientY": 7, "button": 0}),
    );
    let body = r.engine.borrow().doc.body().unwrap();
    let ok = r.dispatch(
        "keydown",
        Some(body),
        &serde_json::json!({"key": "a", "code": "KeyA", "ctrlKey": true}),
    );
    let prevented = !r.dispatch(
        "keydown",
        Some(body),
        &serde_json::json!({"key": "x", "code": "KeyX"}),
    );
    assert!(
        ok && prevented,
        "dispatch reports whether default was prevented"
    );
    assert_eq!(
        s(&mut r, "got"),
        r#"[["pointerdown",12.5,7,0,true,true],["a","KeyA",true,true],["x","KeyX",false,true]]"#
    );
}

#[test]
fn checkbox_radio_label_and_form_activation() {
    let mut r = rt("<form id=f><input id=c type=checkbox><label for=c id=l>lbl</label><input type=radio name=g id=r1 checked><input type=radio name=g id=r2><button id=s>go</button><input id=t value=abc></form>");
    run(
        &mut r,
        "const $ = id => document.getElementById(id); globalThis.log = [];
         for (const id of ['c', 'r1', 'r2']) { $(id).addEventListener('input', () => log.push('input:' + id)); $(id).addEventListener('change', () => log.push('change:' + id + ':' + $(id).checked)); }
         $('f').addEventListener('submit', e => { e.preventDefault(); log.push('submit'); });
         $('l').click(); $('r2').click(); globalThis.state = [$('c').checked, $('r1').checked, $('r2').checked]; $('s').click();
         $('c').addEventListener('click', e => e.preventDefault()); $('c').click(); globalThis.cancelled = $('c').checked;",
    );
    assert_eq!(s(&mut r, "log.join(',')"), "input:c,change:c:true,input:r2,change:r2:true,submit");
    assert_eq!(s(&mut r, "state"), "[true,false,true]", "radio group is exclusive");
    assert_eq!(s(&mut r, "cancelled"), "true", "preventDefault restores the toggle");
}

#[test]
fn form_control_values_selection_and_focus_events() {
    let mut r = rt("<input id=i value='héllo'><textarea id=t>line</textarea><select id=s><option value=a>A<option value=b selected>B</select><button id=b>x</button>");
    run(
        &mut r,
        "const $ = id => document.getElementById(id); globalThis.log = [];
         for (const id of ['i', 'b']) for (const ev of ['focus', 'blur', 'focusin', 'focusout']) $(id).addEventListener(ev, e => log.push(id + ':' + ev + ':' + (e.relatedTarget ? e.relatedTarget.id : '-')));
         $('i').focus(); $('b').focus(); globalThis.active = document.activeElement.id; $('b').blur(); globalThis.afterBlur = document.activeElement === document.body;
         $('i').value = 'new text'; $('i').setSelectionRange(1, 3); globalThis.sel = [$('i').selectionStart, $('i').selectionEnd, $('i').value];
         globalThis.vals = [$('t').value, $('s').value, $('s').selectedIndex, $('i').defaultValue]; $('s').value = 'a'; globalThis.sv = [$('s').value, $('s').selectedIndex];",
    );
    assert_eq!(
        s(&mut r, "log.join(' ')"),
        "i:focus:- i:focusin:- i:blur:b i:focusout:b b:focus:i b:focusin:i b:blur:- b:focusout:-"
    );
    assert_eq!(s(&mut r, "[active, afterBlur, sel, vals, sv]"), r#"["b",true,[1,3,"new text"],["line","b",1,"héllo"],["a",0]]"#);
}

#[test]
fn modules_resolve_relative_imports_and_support_top_level_await() {
    let mut r = rt("");
    r.add_module("main.js", "import { add, PI } from './lib/math.js'; import def, * as ns from './lib/more'; import { version } from 'rendrix'; const v = await Promise.resolve(5); globalThis.out = [add(v, 1), PI, def(), Object.keys(ns).join(), typeof version];".into());
    r.add_module(
        "lib/math.js",
        "export const PI = 3; export function add(a, b) { return a + b; }".into(),
    );
    r.add_module(
        "lib/more.js",
        "import { PI } from './math.js'; export default () => PI * 2; export const extra = 1;"
            .into(),
    );
    r.run_module("main.js").expect("module graph evaluates");
    assert_eq!(s(&mut r, "out"), r#"[6,3,6,"default,extra","string"]"#);
    r.add_module("bad.js", "import './nope.js';".into());
    let e = r.run_module("bad.js").unwrap_err();
    assert!(
        e.message.to_lowercase().contains("nope")
            || e.message.to_lowercase().contains("resolv")
            || e.message.to_lowercase().contains("could not"),
        "{}",
        e.message
    );
    r.add_module(
        "throws.js",
        "throw new TypeError('startup failure');".into(),
    );
    assert!(r
        .run_module("throws.js")
        .unwrap_err()
        .message
        .contains("startup failure"));
}

#[test]
fn runaway_scripts_are_interrupted_inside_the_task_budget() {
    use std::time::{Duration, Instant};
    let mut r = rt("");
    r.set_task_budget(Duration::from_millis(200));
    // A tight loop with no awaits or calls would hang the frame loop without the guard.
    let start = Instant::now();
    let e = r.run_script("loop", "while (true) {}").unwrap_err();
    assert!(
        start.elapsed() < Duration::from_secs(10),
        "interrupt fired instead of hanging"
    );
    assert!(e.message.contains("runaway script interrupted"), "{e}");
    // The engine stays usable after an interrupt: the flag is cleared and no deadline lingers.
    run(&mut r, "globalThis.after = 1 + 1;");
    assert_eq!(s(&mut r, "after"), "2");
    // Microtask storms are preempted the same way (surfaced via take_errors, not a hang).
    r.run_script(
        "storm",
        "Promise.resolve().then(function loop() { return Promise.resolve().then(loop); });",
    )
    .expect("scheduling the storm succeeds");
    assert!(
        r.take_errors()
            .iter()
            .any(|m| m.contains("runaway script interrupted")),
        "storm recorded: {:?}",
        r.take_errors()
    );
    // A pending top-level await still suspends cleanly (WouldBlock), not an error.
    r.add_module(
        "tla.js",
        "await new Promise(res => setTimeout(res, 20)); globalThis.tla = 'done';".into(),
    );
    r.run_module("tla.js")
        .expect("pending TLA suspends without error");
    pump(&mut r, 60);
    assert_eq!(s(&mut r, "tla"), "done");
}

#[test]
fn error_stacks_remap_to_original_sources_via_bundle_maps() {
    let mut r = rt("");
    // Map the compiled module back to its original TS source (synthetic v3 map; deltas
    // accumulate, so the second line uses zero deltas to stay on original line 2).
    r.add_source_map(
        "main.js",
        r#"{"version":3,"sources":["src/main.ts"],"names":[],"mappings":"AACA;AAAA"}"#,
    );
    // Invalid maps and unknown files must not break reporting.
    r.add_source_map("bad.js", "not json");
    r.add_module(
        "main.js",
        "function boom() { throw new Error('kablam'); }\nboom();".into(),
    );
    let e = r.run_module("main.js").unwrap_err();
    assert!(e.message.contains("kablam"), "{e}");
    let stack = e.stack.as_deref().unwrap_or("");
    assert!(
        stack.contains("(src/main.ts:2:"),
        "frames remap to the original source: {stack}"
    );
    assert!(
        !stack.contains("main.js"),
        "no unmapped frames remain: {stack}"
    );
    // Errors without a registered map keep their compiled locations.
    r.add_module("plain.js", "throw new Error('raw');".into());
    let e = r.run_module("plain.js").unwrap_err();
    assert!(
        e.stack.as_deref().unwrap_or("").contains("plain.js:1:"),
        "{e}"
    );
}

#[test]
fn dynamic_import_loads_chunks_with_top_level_await_in_order() {
    let mut r = rt("");
    r.add_module(
        "main.js",
        "globalThis.seq = []; const m = await import('./lib/chunk.js'); seq.push('main:' + m.val);"
            .into(),
    );
    r.add_module(
        "lib/chunk.js",
        "globalThis.seq.push('chunk-eval'); const v = await Promise.resolve(7); globalThis.seq.push('chunk-tla'); export const val = v * 6;".into(),
    );
    r.run_module("main.js").expect("dynamic import evaluates");
    assert_eq!(s(&mut r, "seq"), r#"["chunk-eval","chunk-tla","main:42"]"#);
    // A variable (non-literal) specifier flows through the same loader, resolved against the importer.
    r.add_module("lib/other.js", "export const x = 'loaded';".into());
    r.add_module("loader.js", "import { x } from './lib/other.js'; export async function load(n) { return import(n); } globalThis.staticOther = x;".into());
    r.add_module("main2.js", "import { load } from './loader.js'; const m = await load('./lib/other.js'); globalThis.dyn = m.x;".into());
    r.run_module("main2.js")
        .expect("variable dynamic import evaluates");
    assert_eq!(s(&mut r, "[staticOther, dyn]"), r#"["loaded","loaded"]"#);
}

#[test]
fn jsx_runtime_builds_dom_with_props_events_and_children() {
    let mut r = rt("<div id=root></div>");
    r.add_module(
        "app.js",
        r#"import { jsx, jsxs, Fragment, render } from 'rendrix/jsx-runtime';
           let count = 0;
           const Button = props => jsx('button', { class: 'btn', onClick: props.onClick, children: props.children });
           const App = () => jsxs('div', { className: 'app', style: { color: 'red', '--k': '3px' }, 'data-x': 1, children: [
             jsx('h1', { children: 'Count: ' + count }), jsxs(Fragment, { children: [jsx('span', { children: 'a' }), null, false, jsx('span', { children: 2 })] }),
             jsx(Button, { onClick: () => { count++; }, children: 'inc' }), jsx('input', { value: 'v', disabled: true, ref: el => { globalThis.inputRef = el; } })] });
           render(App(), document.getElementById('root'));
           globalThis.btn = document.querySelector('button');"#
            .into(),
    );
    r.run_module("app.js").unwrap();
    assert_eq!(
        s(&mut r, "document.getElementById('root').innerHTML"),
        r#"<div class="app" style="color: red; --k: 3px;" data-x="1"><h1>Count: 0</h1><span>a</span><span>2</span><button class="btn">inc</button><input value="v" disabled></div>"#
    );
    run(&mut r, "btn.click();");
    assert_eq!(s(&mut r, "inputRef.value"), "v");
    assert_eq!(s(&mut r, "document.querySelector('.app').style.color"), "red");
}

#[test]
fn url_search_params_encoding_and_base64() {
    let mut r = rt("");
    run(
        &mut r,
        "const u = new URL('https://user:pw@Example.com:8080/a/b/../c?x=1&y=two%20words#frag'); const rel = new URL('../d?q=1', 'http://h.test/a/b/c'); const sp = new URLSearchParams({ a: '1 2', b: 'x&y' }); sp.append('a', '3');
         u.searchParams.set('z', 'é'); globalThis.res = { href: u.href, origin: u.origin, host: u.host, path: u.pathname, y: u.searchParams.get('y'), rel: rel.href, sp: sp.toString(), all: sp.getAll('a'), canParse: [URL.canParse('nope'), URL.canParse('/x', 'http://a')],
           enc: [...new TextEncoder().encode('héllo €😀')].join(' '), dec: new TextDecoder().decode(new TextEncoder().encode('héllo €😀')), b64: [btoa('hello'), atob('aGVsbG8=')] };",
    );
    assert_eq!(
        s(&mut r, "res"),
        r#"{"href":"https://user:pw@example.com:8080/a/c?x=1&y=two+words&z=%C3%A9#frag","origin":"https://example.com:8080","host":"example.com:8080","path":"/a/c","y":"two words","rel":"http://h.test/a/d?q=1","sp":"a=1+2&b=x%26y&a=3","all":["1 2","3"],"canParse":[false,true],"enc":"104 195 169 108 108 111 32 226 130 172 240 159 152 128","dec":"héllo €😀","b64":["aGVsbG8=","hello"]}"#
    );
}

#[test]
fn abort_controller_and_structured_clone() {
    let mut r = rt("");
    run(
        &mut r,
        "const c = new AbortController(); globalThis.log = []; c.signal.addEventListener('abort', () => log.push('abort')); c.abort(); c.abort(); globalThis.aborted = [c.signal.aborted, c.signal.reason.name];
         const src = { a: [1, { b: new Date(5) }], m: new Map([[1, new Set([2])]]), n: null }; src.self = src; const copy = structuredClone(src);
         globalThis.cl = [copy !== src, copy.self === copy, copy.a[1].b.getTime(), copy.m.get(1).has(2), (() => { try { structuredClone({ f() {} }); } catch (e) { return e.name; } })()];",
    );
    assert_eq!(s(&mut r, "[log, aborted, cl]"), r#"[["abort"],[true,"AbortError"],[true,true,5,true,"DataCloneError"]]"#);
}

#[test]
fn fetch_reads_bundled_assets_and_data_urls_and_aborts() {
    let mut assets = MapAssets::default();
    assets.0.insert(
        "data/info.json".into(),
        br#"{"name":"rendrix","n":[1,2]}"#.to_vec(),
    );
    let mut r = rt_with("", assets);
    run(
        &mut r,
        "globalThis.res = {};
         fetch('data/info.json').then(r => { res.status = r.status; res.ct = r.headers.get('content-type'); return r.json(); }).then(j => { res.json = j; });
         fetch('data:text/plain;base64,aGk=').then(r => r.text()).then(t => { res.data = t; });
         fetch('missing.txt').then(r => { res.missing = [r.ok, r.status]; });
         const c = new AbortController(); fetch('data/info.json', { signal: c.signal }).catch(e => { res.abort = e.name; }); c.abort();
         fetch('./data/info.json', { method: 'POST', body: 'x' }).then(r => r.text()).then(t => { res.post = t.length; });",
    );
    pump(&mut r, 50);
    assert_eq!(
        s(&mut r, "Object.keys(res).sort().map(k => [k, res[k]])"),
        r#"[["abort","AbortError"],["ct","application/json"],["data","hi"],["json",{"name":"rendrix","n":[1,2]}],["missing",[false,404]],["post",28],["status",200]]"#
    );
}

#[test]
fn local_and_session_storage() {
    let mut r = rt("");
    run(&mut r, "localStorage.clear(); localStorage.setItem('a', 1); localStorage.b = 'two'; sessionStorage.s = 'x'; globalThis.out = [localStorage.getItem('a'), localStorage.b, localStorage.length, Object.keys(localStorage), 'a' in localStorage, localStorage.missing, sessionStorage.s]; delete localStorage.a; globalThis.out2 = [localStorage.length, localStorage.key(0)];");
    assert_eq!(s(&mut r, "out"), r#"["1","two",2,["a","b"],true,null,"x"]"#);
    assert_eq!(s(&mut r, "out2"), r#"[1,"b"]"#);
}

#[test]
fn document_cookie_syncs_with_the_session_jar_and_honours_expiry() {
    let mut r = rt("");
    run(&mut r, "globalThis.r1 = [navigator.cookieEnabled, document.cookie];
      document.cookie = 'a=1'; document.cookie = 'b=two; Path=/';
      globalThis.r2 = [document.cookie, document.cookie];
      document.cookie = 'a=gone; Max-Age=0';
      globalThis.r3 = document.cookie;
      document.cookie = 'old=x; Expires=Wed, 21 Oct 2015 07:28:00 GMT';
      globalThis.r4 = document.cookie;");
    assert_eq!(s(&mut r, "r1"), r#"[true,""]"#);
    assert_eq!(s(&mut r, "r2"), r#"["a=1; b=two","a=1; b=two"]"#);
    assert_eq!(s(&mut r, "r3"), "b=two");
    assert_eq!(s(&mut r, "r4"), "b=two");
}

#[test]
fn mutation_observer_delivers_batched_records() {
    let mut r = rt("<div id=root><p id=p>x</p></div>");
    run(
        &mut r,
        "globalThis.recs = []; const root = document.getElementById('root');
         new MutationObserver(rs => { for (const m of rs) recs.push([m.type, m.target.id || m.target.nodeName, m.addedNodes.length, m.removedNodes.length, m.attributeName, m.oldValue]); })
           .observe(root, { childList: true, attributes: true, subtree: true, attributeOldValue: true, characterData: true });
         const p = document.getElementById('p'); p.setAttribute('title', 't1'); p.setAttribute('title', 't2'); root.appendChild(document.createElement('span')); p.firstChild.data = 'y'; root.removeChild(p);",
    );
    assert_eq!(
        s(&mut r, "recs"),
        r##"[["attributes","p",0,0,"title",null],["attributes","p",0,0,"title","t1"],["childList","root",1,0,null,null],["characterData","#text",0,0,null,null],["childList","root",0,1,null,null]]"##
    );
}

#[test]
fn resize_observer_and_media_queries_follow_the_viewport() {
    let mut r = rt("<style>body{margin:0} #a{width:50px;height:20px}</style><div id=a></div>");
    run(
        &mut r,
        "globalThis.sizes = []; globalThis.mq = []; const a = document.getElementById('a');
         new ResizeObserver(es => es.forEach(e => sizes.push([e.contentRect.width, e.contentRect.height]))).observe(a);
         const m = matchMedia('(max-width: 300px)'); mq.push(m.matches); m.addEventListener('change', e => mq.push(e.matches)); addEventListener('resize', () => mq.push('resized'));",
    );
    r.run_animation_frame(16.0);
    a_resize(&mut r);
    r.engine.borrow_mut().set_viewport(250.0, 300.0, 1.0);
    r.viewport_changed();
    assert_eq!(s(&mut r, "sizes"), "[[50,20],[80,20]]");
    assert_eq!(s(&mut r, "mq"), r#"[false,"resized",true]"#);
    assert_eq!(s(&mut r, "[innerWidth, innerHeight, devicePixelRatio]"), "[250,300,1]");
}

fn a_resize(r: &mut JsRuntime) {
    run(r, "document.getElementById('a').style.width = '80px';");
    r.run_animation_frame(32.0);
}

#[test]
fn resize_observer_box_options_report_distinct_boxes() {
    let mut r = rt("<style>body{margin:0} #b{width:100px;height:40px;padding:10px;border:5px solid black}</style><div id=b></div>");
    run(
        &mut r,
        "globalThis.rec = []; const b = document.getElementById('b');
         const o1 = new ResizeObserver(es => es.forEach(e => rec.push(['content', e.contentRect.width, e.contentRect.height, e.contentBoxSize[0].inlineSize, e.borderBoxSize[0].inlineSize]))); o1.observe(b);
         const o2 = new ResizeObserver(es => es.forEach(e => rec.push(['border', e.contentRect.width, e.borderBoxSize[0].inlineSize, e.borderBoxSize[0].blockSize]))); o2.observe(b, { box: 'border-box' });
         const o3 = new ResizeObserver(es => es.forEach(e => rec.push(['device', e.devicePixelContentBoxSize[0].inlineSize, e.devicePixelContentBoxSize[0].blockSize]))); o3.observe(b, { box: 'device-pixel-content-box' });",
    );
    r.run_animation_frame(16.0);
    // content-box 100x40, border-box 130x70 (10px padding + 5px border per side).
    assert_eq!(s(&mut r, "rec"), "[[\"content\",100,40,100,130],[\"border\",100,130,70],[\"device\",100,40]]");
    // Invalid box options throw.
    run(
        &mut r,
        "globalThis.boxErr = (() => { try { new ResizeObserver(() => {}).observe(b, { box: 'margin-box' }); return 'no-throw'; } catch (e) { return e.name; } })();",
    );
    assert_eq!(s(&mut r, "boxErr"), "TypeError");
    // Growing the content box fires content/border observers with the new sizes.
    run(&mut r, "rec.length = 0; document.getElementById('b').style.width = '120px';");
    r.run_animation_frame(32.0);
    assert_eq!(s(&mut r, "rec"), "[[\"content\",120,40,120,150],[\"border\",120,150,70],[\"device\",120,40]]");
    // devicePixelContentBoxSize scales with the viewport scale.
    r.engine.borrow_mut().set_viewport(400.0, 300.0, 2.0);
    run(&mut r, "rec.length = 0; document.getElementById('b').style.height = '50px';");
    r.run_animation_frame(48.0);
    assert!(
        s(&mut r, "rec").contains("[\"device\",240,100]"),
        "device pixels scale with dpr: {}",
        s(&mut r, "rec")
    );
}

#[test]
fn intersection_observer_delivers_per_frame_entries() {
    let mut r = rt("<style>body{margin:0} #a{width:100px;height:100px} #far{margin-top:900px;width:50px;height:50px}</style><div id=a></div><div id=far></div>");
    run(
        &mut r,
        "globalThis.entries = []; const a = document.getElementById('a'), far = document.getElementById('far');
         globalThis.io = new IntersectionObserver(es => { for (const e of es) entries.push([e.target.id, e.isIntersecting, Math.round(e.intersectionRatio * 100) / 100]); });
         io.observe(a); io.observe(far);",
    );
    assert_eq!(s(&mut r, "typeof IntersectionObserver"), "function");
    r.run_animation_frame(16.0);
    assert_eq!(s(&mut r, "entries"), r#"[["a",true,1],["far",false,0]]"#, "initial state fires once per target");
    // No changes -> no further callbacks (per-frame delivery, no polling spam).
    run(&mut r, "entries.length = 0;");
    r.run_animation_frame(32.0);
    assert_eq!(s(&mut r, "entries"), "[]");
    // Hiding the element flips it to non-intersecting on the next frame.
    run(&mut r, "entries.length = 0; document.getElementById('a').style.display = 'none';");
    r.run_animation_frame(48.0);
    assert_eq!(s(&mut r, "entries"), r#"[["a",false,0]]"#);
    // Unobserve stops delivery; disconnect clears the rest.
    run(&mut r, "entries.length = 0; io.unobserve(document.getElementById('far')); document.getElementById('a').style.display = 'block';");
    r.run_animation_frame(64.0);
    assert_eq!(s(&mut r, "entries"), r#"[["a",true,1]]"#, "far was unobserved");
    run(&mut r, "entries.length = 0; io.disconnect(); document.getElementById('a').style.display = 'none';");
    r.run_animation_frame(80.0);
    assert_eq!(s(&mut r, "entries"), "[]", "disconnected observer stays silent");
    // Constructor validation.
    run(
        &mut r,
        "globalThis.ioErrs = [];
         try { new IntersectionObserver('nope'); } catch (e) { ioErrs.push(e.name); }
         try { new IntersectionObserver(() => {}, { threshold: 2 }); } catch (e) { ioErrs.push(e.name); }
         try { new IntersectionObserver(() => {}, { rootMargin: 'bogus' }); } catch (e) { ioErrs.push(e.name); }
         try { new IntersectionObserver(() => {}).observe(null); } catch (e) { ioErrs.push(e.name); }",
    );
    assert_eq!(s(&mut r, "ioErrs"), r#"["TypeError","TypeError","TypeError","TypeError"]"#);
}

#[test]
fn intersection_observer_thresholds_and_root_margin() {
    let mut r = rt("<style>body{margin:0} #edge{margin-left:350px;width:100px;height:50px}</style><div id=edge></div>");
    run(
        &mut r,
        "globalThis.ratios = []; const edge = document.getElementById('edge');
         globalThis.io2 = new IntersectionObserver(es => { for (const e of es) ratios.push(Math.round(e.intersectionRatio * 100) / 100); }, { threshold: [0, 0.5, 1] });
         io2.observe(edge);",
    );
    r.run_animation_frame(16.0);
    // Viewport is 400 wide; the box spans 350..450 so 50/100 = 0.5 is visible.
    assert_eq!(s(&mut r, "ratios"), "[0.5]");
    // A large rootMargin pulls the box fully inside.
    run(
        &mut r,
        "ratios.length = 0; globalThis.io3 = new IntersectionObserver(es => { for (const e of es) ratios.push([e.isIntersecting, Math.round(e.intersectionRatio * 100) / 100]); }, { rootMargin: '100px' }); io3.observe(edge);",
    );
    r.run_animation_frame(32.0);
    assert!(
        s(&mut r, "ratios").contains("[true,1]"),
        "rootMargin expands the clip: {}",
        s(&mut r, "ratios")
    );
}

#[test]
fn computed_style_covers_common_properties() {
    let mut r = rt("<style>body{margin:0} #x{width:120px;height:40px;margin:10px 20px;padding:5px;color:rgb(1,2,3);background-color:rgb(4,5,6);display:flex;position:absolute;top:3px;left:4px;border:2px solid rgb(7,8,9);border-radius:6px;font-style:italic;font-weight:700;opacity:0.5;overflow:hidden;z-index:7;flex-direction:column;box-sizing:border-box}</style><div id=x></div>");
    run(
        &mut r,
        "const cs = getComputedStyle(document.getElementById('x'));
         globalThis.c = { d: cs.display, pos: cs.position, w: cs.width, h: cs.height, mt: cs.marginTop, m: cs.margin, pt: cs.paddingTop, p: cs.padding,
           btw: cs.borderTopWidth, bts: cs.borderTopStyle, btc: cs.borderTopColor, br: cs.borderTopLeftRadius, bw: cs.borderWidth, bs: cs.borderStyle,
           box: cs.boxSizing, fw: cs.fontWeight, fs: cs.fontStyle, fam: cs.fontFamily, lh: cs.lineHeight, ls: cs.letterSpacing,
           o: cs.opacity, of: cs.overflow, ox: cs.overflowX, zi: cs.zIndex, fd: cs.flexDirection, fg: cs.flexGrow, tr: cs.transform, cur: cs.cursor, pe: cs.pointerEvents,
           bg: cs.backgroundColor, col: cs.color, ta: cs.textAlign, tt: cs.textTransform, ws: cs.whiteSpace, td: cs.textDecorationLine, us: cs.userSelect };",
    );
    assert_eq!(
        s(&mut r, "c"),
        r#"{"d":"flex","pos":"absolute","w":"120px","h":"40px","mt":"10px","m":"10px 20px","pt":"5px","p":"5px","btw":"2px","bts":"solid","btc":"rgb(7, 8, 9)","br":"6px","bw":"2px","bs":"solid","box":"border-box","fw":"700","fs":"italic","fam":"sans-serif","lh":"normal","ls":"0px","o":"0.5","of":"hidden","ox":"hidden","zi":"7","fd":"column","fg":"0","tr":"none","cur":"auto","pe":"auto","bg":"rgb(4, 5, 6)","col":"rgb(1, 2, 3)","ta":"start","tt":"none","ws":"normal","td":"none","us":"auto"}"#
    );
    // Custom properties still pass through, unknown properties stay empty.
    run(&mut r, "document.getElementById('x').style.setProperty('--brand', 'teal'); globalThis.custom = [getComputedStyle(document.getElementById('x')).getPropertyValue('--brand'), getComputedStyle(document.getElementById('x')).getPropertyValue('nope-not-real')];");
    assert_eq!(s(&mut r, "custom"), r#"["teal",""]"#);
}

#[test]
fn unhandled_rejections_and_script_errors_are_reported() {
    let mut r = rt("");
    run(&mut r, "globalThis.seen = []; addEventListener('unhandledrejection', e => { seen.push(String(e.reason)); if (String(e.reason).includes('nobody')) e.preventDefault(); }); Promise.reject(new Error('nobody catches this'));");
    assert_eq!(s(&mut r, "seen.length"), "1");
    run(&mut r, "Promise.reject('plain');");
    assert!(logs(&mut r).iter().any(|l| l.contains("Uncaught (in promise)") && l.contains("plain")));
    let e = r.run_script("bad", "throw new RangeError('x is out of range');").unwrap_err();
    assert!(e.message.contains("x is out of range"), "{e}");
    assert!(r.run_script("syntax", "let = ;").is_err());
}

#[test]
fn console_formatting() {
    let mut r = rt("");
    run(&mut r, "console.log('a', 1, { b: [1, 2], c: 'x' }, null, undefined); console.warn('%s is %d years', 'Ann', 30, 'extra'); console.group('G'); console.info('in'); console.groupEnd(); console.error(new Error('bad'));");
    let l = logs(&mut r);
    assert_eq!(l[0], "log:a 1 { b: [ 1, 2 ], c: \"x\" } null undefined");
    assert_eq!(l[1], "warn:Ann is 30 years extra");
    assert_eq!(l[2], "log:G");
    assert_eq!(l[3], "info:  in");
    assert!(l[4].starts_with("error:Error: bad"), "{}", l[4]);
}

#[test]
fn wrappers_are_collected_and_native_nodes_freed() {
    let mut r = rt("<div id=host></div>");
    run(&mut r, "const host = document.getElementById('host'); for (let i = 0; i < 600; i++) { const d = document.createElement('div'); d.textContent = 'n' + i; host.appendChild(d); host.removeChild(d); }");
    r.collect_garbage();
    run(&mut r, "1;"); // finalizer callbacks run as jobs after a collection
    r.collect_garbage();
    run(&mut r, "1;");
    r.engine.borrow_mut().doc.collect_garbage();
    let live = r.engine.borrow().doc.node_count();
    assert!(live < 40, "600 created+removed elements (and their text nodes) must not accumulate: {live} nodes alive");
    // A node that is still referenced from script survives sweeps, with its listeners and identity.
    run(&mut r, "globalThis.kept = document.createElement('p'); kept.addEventListener('x', () => { globalThis.fired = true; });");
    r.collect_garbage();
    run(&mut r, "1;");
    r.engine.borrow_mut().doc.collect_garbage();
    run(&mut r, "kept.dispatchEvent(new Event('x'));");
    assert_eq!(s(&mut r, "fired"), "true");
    assert_eq!(s(&mut r, "document.getElementById('host') === document.getElementById('host')"), "true");
}

#[test]
fn unsupported_apis_are_honestly_absent_or_throw() {
    let mut r = rt("<canvas id=c width=10 height=10></canvas>");
    assert_eq!(
        s(&mut r, "[typeof WebSocket, typeof XMLHttpRequest, typeof IntersectionObserver]"),
        r#"["function","undefined","function"]"#
    );
    assert_eq!(
        s(&mut r, "[typeof WebGLRenderingContext, typeof WebGL2RenderingContext, typeof RendrixScene, typeof Rendrix3D]"),
        r#"["function","function","object","object"]"#
    );
    assert_eq!(
        s(&mut r, "typeof document.getElementById('c').getContext('2d')"),
        "object"
    );
    run(&mut r, "globalThis.e = (() => { try { const s = document.body.attachShadow({ mode: 'open' }); return s && s.host === document.body && document.body.shadowRoot === s ? 'attached' : 'bad-root'; } catch (e) { return e.name; } })();");
    assert_eq!(s(&mut r, "e"), "attached");
    run(&mut r, "globalThis.e2 = (() => { try { document.body.attachShadow({ mode: 'open' }); return 'second-ok'; } catch (e) { return e.name; } })();");
    assert_eq!(s(&mut r, "e2"), "NotSupportedError");
}

#[test]
fn form_validation_reflects_constraints_and_invalid_events() {
    let mut r = rt(
        "<form id=f><input id=req required value=''><input id=em type=email value='nope'><input id=num type=number min=5 value='3'></form>",
    );
    // Required empty fails with valueMissing; invalid event fires on checkValidity.
    assert_eq!(s(&mut r, "document.getElementById('req').validity.valueMissing"), "true");
    assert_eq!(s(&mut r, "document.getElementById('req').checkValidity()"), "false");
    assert_eq!(s(&mut r, "document.getElementById('em').validity.typeMismatch"), "true");
    assert_eq!(s(&mut r, "document.getElementById('num').validity.rangeUnderflow"), "true");
    assert_eq!(s(&mut r, "document.getElementById('f').checkValidity()"), "false");
    run(&mut r, "document.getElementById('req').value = 'x'; document.getElementById('em').value = 'a@b.co'; document.getElementById('num').value = '7';");
    assert_eq!(s(&mut r, "document.getElementById('f').checkValidity()"), "true");
    // Custom errors fail until cleared.
    run(&mut r, "const i = document.getElementById('req'); i.setCustomValidity('nope');");
    assert_eq!(s(&mut r, "document.getElementById('req').validity.customError"), "true");
    assert_eq!(s(&mut r, "document.getElementById('req').checkValidity()"), "false");
    run(&mut r, "document.getElementById('req').setCustomValidity('');");
    assert_eq!(s(&mut r, "document.getElementById('req').checkValidity()"), "true");
}

#[test]
fn range_selection_custom_elements_and_contenteditable_work() {
    let mut r = rt("<div id=a>hello</div><div id=ed>edit me</div>");
    // Range over a text node.
    assert_eq!(
        s(&mut r, "(() => { const t = document.getElementById('a').firstChild; const rg = document.createRange(); rg.setStart(t, 1); rg.setEnd(t, 4); return rg.toString(); })()"),
        "ell"
    );
    // Selection singleton.
    assert_eq!(
        s(&mut r, "(() => { const sel = document.getSelection(); sel.removeAllRanges(); const rg = document.createRange(); rg.selectNodeContents(document.getElementById('a')); sel.addRange(rg); return [sel.rangeCount, sel.toString()].join('|'); })()"),
        "1|hello"
    );
    // Custom elements registry + upgrade on createElement.
    assert_eq!(
        s(&mut r, "(() => { class X extends HTMLElement {} customElements.define('my-x', X); const el = document.createElement('my-x'); return [customElements.get('my-x') === X, el instanceof X].join('|'); })()"),
        "true|true"
    );
    // Duplicate define throws; bad name throws SyntaxError.
    assert_eq!(
        s(&mut r, "(() => { const out = []; try { customElements.define('my-x', class extends HTMLElement {}); } catch (e) { out.push(e.name); } try { customElements.define('notacustomname', class extends HTMLElement {}); } catch (e) { out.push(e.name); } return out.join('|'); })()"),
        "NotSupportedError|SyntaxError"
    );
    // contenteditable reflection + designMode fallback.
    assert_eq!(s(&mut r, "document.getElementById('ed').isContentEditable"), "false");
    run(&mut r, "document.getElementById('ed').setAttribute('contenteditable', 'true');");
    assert_eq!(s(&mut r, "document.getElementById('ed').isContentEditable"), "true");
    assert_eq!(s(&mut r, "(() => { document.designMode = 'on'; const d = document.createElement('div'); return d.isContentEditable; })()"), "true");
}

#[test]
fn filesystem_access_is_permission_gated() {
    let dir = std::env::temp_dir().join(format!("rendrix-fs-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("a.txt"), "hello").unwrap();
    let mut r = rt("");
    r.host.borrow_mut().permissions = Permissions {
        network: false,
        fs_read: vec![dir.clone()],
        fs_write: vec![],
    };
    let p = dir.join("a.txt").to_string_lossy().replace('\\', "/");
    run(
        &mut r,
        &format!(
            "globalThis.res = []; rendrix.fs.readTextFile('{p}').then(t => res.push(t)); rendrix.fs.writeTextFile('{p}', 'x').catch(e => res.push('write denied'));
             rendrix.fs.readTextFile('C:/Windows/win.ini').catch(e => res.push('read denied')); fetch('https://example.com').catch(e => res.push('net denied'));"
        ),
    );
    pump(&mut r, 30);
    let out = s(&mut r, "res");
    assert!(out.contains("hello") && out.contains("write denied") && out.contains("read denied") && out.contains("net denied"), "{out}");
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn crypto_random_values_come_from_the_os_and_look_random() {
    let mut r = rt("");
    run(&mut r, "const a = crypto.randomUUID(), b = crypto.randomUUID(); const buf = crypto.getRandomValues(new Uint8Array(4096)); const distinct = new Set(buf).size;
                 globalThis.out = [/^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/.test(a), a !== b, distinct > 200];");
    assert_eq!(s(&mut r, "out"), "[true,true,true]");
}

#[test]
fn crypto_subtle_digest_supports_the_sha_family() {
    let mut r = rt("");
    run(
        &mut r,
        "globalThis.digests = {};
         const data = new TextEncoder().encode('abc');
         for (const alg of ['SHA-1', 'SHA-256', 'SHA-384', 'SHA-512']) {
           crypto.subtle.digest(alg, data).then(buf => {
             digests[alg] = [...new Uint8Array(buf)].map(x => x.toString(16).padStart(2, '0')).join('');
           });
         }
         crypto.subtle.digest({name:'sha-256'}, data.buffer).then(buf => { digests.buffer = new Uint8Array(buf).length; });",
    );
    pump(&mut r, 30);
    assert_eq!(s(&mut r, "digests['SHA-1']"), "a9993e364706816aba3e25717850c26c9cd0d89d");
    assert_eq!(s(&mut r, "digests['SHA-256']"), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
    assert_eq!(s(&mut r, "digests['SHA-384']"), "cb00753f45a35e8bb5a03d699ac65007272c32ab0eded1631a8b605a43ff5bed8086072ba1e7cc2358baeca134c825a7");
    assert_eq!(s(&mut r, "digests['SHA-512'].length"), "128");
    assert_eq!(s(&mut r, "digests.buffer"), "32");
}

// ------------------------------------------------------------------------------------
// Dedicated workers (A4)
// ------------------------------------------------------------------------------------

/// Worker-aware pump: outbox → threads → capped delivery, plus normal timers.
fn wpump(rt: &mut JsRuntime, ms: u64) {
    let end = std::time::Instant::now() + std::time::Duration::from_millis(ms);
    while std::time::Instant::now() < end {
        rt.poll_workers();
        rt.tick();
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
    rt.poll_workers();
    rt.tick();
}

fn wrt(modules: &[(&str, &str)]) -> JsRuntime {
    let mut r = rt("");
    let map: std::collections::HashMap<String, String> = modules
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    for (k, v) in &map {
        r.add_module(k, v.clone());
    }
    r.set_worker_modules(map);
    r
}

#[test]
fn worker_round_trip_echo_with_timers_and_url_forms() {
    let mut r = wrt(&[(
        "worker.js",
        "onmessage = e => setTimeout(() => postMessage({ echo: e.data }), 5);",
    )]);
    run(
        &mut r,
        "globalThis.got = []; globalThis.errs = [];
         const mk = u => { const w = new Worker(u); w.onmessage = e => got.push(e.data); w.onerror = e => errs.push(e.message); return w; };
         globalThis.w1 = mk('worker.js');
         globalThis.w2 = mk(new URL('worker.js', location.href));
         w1.postMessage({ a: 1 }); w2.postMessage([1, 'x']);",
    );
    wpump(&mut r, 400);
    assert_eq!(
        s(&mut r, "errs"),
        "[]",
        "no worker errors: {:?}",
        s(&mut r, "errs")
    );
    assert_eq!(
        s(&mut r, "got.map(x => JSON.stringify(x)).sort()"),
        r#"["{\"echo\":[1,\"x\"]}","{\"echo\":{\"a\":1}}"]"#
    );
    run(&mut r, "w1.terminate(); w2.terminate();");
    wpump(&mut r, 200);
    assert!(!r.worker_alive(), "terminated workers reap their threads");
}

#[test]
fn worker_structured_clone_binary_payloads() {
    let mut r = wrt(&[(
        "worker.js",
        "onmessage = e => {
           const d = e.data;
           postMessage({ n: d.n, neg: d.neg, nan: d.nan, big: d.big, date: d.date.getTime(), re: [d.re.source, d.re.flags],
             m: [...d.m.entries()].map(([k, v]) => [k, [...v]]), s: [...d.s], u8: [...d.u8], f64: [...d.f64],
             nested: d.nested, err: [d.err.name, d.err.message], circ: d.circ.self === d.circ });
         };",
    )]);
    run(
        &mut r,
        "globalThis.got = null; globalThis.errs = [];
         const w = new Worker('worker.js'); w.onmessage = e => { got = e.data; }; w.onerror = e => errs.push(e.message);
         const circ = {}; circ.self = circ;
         w.postMessage({ n: 42, neg: -Infinity, nan: NaN, big: 1000000000000000000000000000000n, date: new Date(123456789), re: /ab+c/gi,
           m: new Map([[1, new Set([2, 3])]]), s: new Set(['a', 1]), u8: new Uint8Array([1, 2, 250]), f64: new Float64Array([0.5, -1.25]),
           nested: { a: [{ b: [true, null, 'x'] }] }, err: new TypeError('kablam'), circ });",
    );
    wpump(&mut r, 500);
    assert_eq!(s(&mut r, "errs"), "[]");
    assert_eq!(s(&mut r, "got.n"), "42");
    assert_eq!(
        s(
            &mut r,
            "[got.neg === -Infinity, Number.isNaN(got.nan), got.big.toString(), got.date]"
        ),
        r#"[true,true,"1000000000000000000000000000000",123456789]"#
    );
    assert_eq!(s(&mut r, "JSON.stringify([got.re, got.m, got.s, got.u8, got.f64, got.nested, got.err, got.circ])"), r#"[["ab+c","gi"],[[1,[2,3]]],["a",1],[1,2,250],[0.5,-1.25],{"a":[{"b":[true,null,"x"]}]},["TypeError","kablam"],true]"#);
    run(&mut r, "w.terminate();");
    wpump(&mut r, 150);
}

#[test]
fn worker_error_event_on_throw() {
    let mut r = wrt(&[(
        "worker.js",
        "onmessage = e => { if (e.data === 'throw') throw new Error('handler-fail-515'); postMessage('ok:' + e.data); };",
    )]);
    run(
        &mut r,
        "globalThis.got = []; globalThis.errs = [];
         const w = new Worker('worker.js'); w.onmessage = e => got.push(e.data); w.onerror = e => errs.push(e.message);
         w.postMessage('first');",
    );
    wpump(&mut r, 300);
    assert_eq!(s(&mut r, "got"), r#"["ok:first"]"#);
    run(&mut r, "w.postMessage('throw');");
    wpump(&mut r, 300);
    assert!(
        s(&mut r, "errs").contains("handler-fail-515"),
        "handler throw surfaces as error event: {}",
        s(&mut r, "errs")
    );
    // The worker survives a handler throw.
    run(&mut r, "w.postMessage('again');");
    wpump(&mut r, 300);
    assert!(
        s(&mut r, "got").contains("ok:again"),
        "worker survives: {}",
        s(&mut r, "got")
    );

    // A worker whose initial evaluation throws reports the error and stays alive.
    let mut r2 = wrt(&[("bad.js", "throw new TypeError('top-level-boom-77');")]);
    run(
        &mut r2,
        "globalThis.errs2 = []; const w2 = new Worker('bad.js'); w2.onerror = e => errs2.push(e.message);",
    );
    wpump(&mut r2, 300);
    assert!(
        s(&mut r2, "errs2").contains("top-level-boom-77"),
        "{}",
        s(&mut r2, "errs2")
    );
    run(&mut r, "w.terminate();");
    run(&mut r2, "w2.terminate();");
    wpump(&mut r, 150);
    wpump(&mut r2, 150);
}

#[test]
fn worker_terminate_then_post_does_not_crash() {
    let mut r = wrt(&[("worker.js", "onmessage = e => postMessage(e.data);")]);
    run(
        &mut r,
        "globalThis.got = []; globalThis.errs = [];
         const w = new Worker('worker.js'); w.onmessage = e => got.push(e.data); w.onerror = e => errs.push('ERR:' + e.message);
         w.postMessage('before');",
    );
    wpump(&mut r, 200);
    run(
        &mut r,
        "w.terminate(); w.postMessage('after-terminate'); w.postMessage({ x: 1 }); w.terminate();",
    );
    wpump(&mut r, 300);
    assert_eq!(
        s(&mut r, "errs"),
        "[]",
        "no errors from post-after-terminate"
    );
    assert!(!r.worker_alive());
    assert_eq!(
        s(&mut r, "__rendrix_workerLiveCount()"),
        "0",
        "main-side handles are released"
    );
}

#[test]
fn worker_fifty_create_terminate_cycles_do_not_leak() {
    let mut r = wrt(&[("worker.js", "onmessage = e => postMessage({ n: e.data });")]);
    run(
        &mut r,
        "globalThis.got = []; globalThis.errs = [];
         globalThis.cycle = i => { const w = new Worker('worker.js'); w.onmessage = e => got.push(e.data.n); w.onerror = e => errs.push(e.message); w.postMessage(i); return w; };",
    );
    for i in 0..50 {
        run(&mut r, &format!("globalThis.wc = cycle({i});"));
        // Wait for the echo (deterministic) instead of an arbitrary sleep: spawn + QuickJS init
        // latency varies, and terminating before the first round trip proves nothing about leaks.
        let end = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while s(&mut r, "String(got[got.length - 1])") != i.to_string()
            && std::time::Instant::now() < end
        {
            wpump(&mut r, 10);
        }
        run(&mut r, "wc.terminate();");
        wpump(&mut r, 10);
    }
    wpump(&mut r, 600);
    assert_eq!(s(&mut r, "errs"), "[]", "{}", s(&mut r, "errs"));
    assert!(!r.worker_alive(), "all 50 workers reaped");
    assert_eq!(s(&mut r, "__rendrix_workerLiveCount()"), "0");
    assert_eq!(
        s(&mut r, "got.length > 40"),
        "true",
        "most echoes landed before termination (got {} echoes)",
        s(&mut r, "got.length")
    );
    // The runtime still works afterwards (ids recycled, no poisoning).
    run(&mut r, "globalThis.final = null; const w = new Worker('worker.js'); w.onmessage = e => { final = e.data; }; w.postMessage('done');");
    wpump(&mut r, 300);
    assert_eq!(s(&mut r, "final"), r#"{"n":"done"}"#);
    run(&mut r, "w.terminate();");
    wpump(&mut r, 150);
}

#[test]
fn worker_busy_worker_does_not_stall_the_frame_pump() {
    let mut r = wrt(&[(
        "worker.js",
        "onmessage = () => { for (let i = 0; i < 200; i++) postMessage({ i }); };",
    )]);
    run(
        &mut r,
        "globalThis.count = 0; const w = new Worker('worker.js'); w.onmessage = () => { count++; }; w.postMessage('go');",
    );
    // Each pump delivers at most WORKER_MAX_PER_TICK events and returns promptly even while the
    // worker floods the channel — this is the frame-stall assertion (tick-count, not wall-clock).
    // A wall-time budget (not a pump count) bounds the loop: thread spawn + QuickJS init alone
    // cost tens of ms, so a tight pump loop would starve the worker before it posts anything.
    let mut polls = 0;
    let start = std::time::Instant::now();
    while s(&mut r, "count") != "200" && start.elapsed() < std::time::Duration::from_secs(10) {
        let t = std::time::Instant::now();
        let delivered = r.poll_workers().len();
        r.tick();
        assert!(delivered <= 16, "per-frame cap respected (got {delivered})");
        assert!(
            t.elapsed() < std::time::Duration::from_millis(500),
            "a single pump never stalls the frame loop"
        );
        polls += 1;
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    let elapsed = start.elapsed();
    assert_eq!(
        s(&mut r, "count"),
        "200",
        "all 200 messages eventually delivered ({polls} pumps)"
    );
    assert!(
        polls > 12,
        "delivery was actually spread across frames ({polls})"
    );
    assert!(
        elapsed < std::time::Duration::from_secs(10),
        "no stall: {elapsed:?}"
    );
    run(&mut r, "w.terminate();");
    wpump(&mut r, 150);
}

#[test]
fn worker_has_no_dom_access_and_no_nested_workers() {
    let mut r = wrt(&[(
        "worker.js",
        "const results = [];
         for (const [name, fn] of [['document', () => document.createElement('div')], ['window', () => window.alert(1)], ['localStorage', () => localStorage.getItem('a')], ['rAF', () => requestAnimationFrame(() => {})]]) {
           try { fn(); results.push(name + ':NO-THROW'); } catch (e) { results.push(name + ':' + e.message); }
         }
         try { new Worker('x.js'); results.push('nested:NO-THROW'); } catch (e) { results.push('nested:' + e.name); }
         try { importScripts('x.js'); results.push('importScripts:NO-THROW'); } catch (e) { results.push('importScripts:' + e.message.slice(0, 60)); }
         postMessage(results);",
    )]);
    run(
        &mut r,
        "globalThis.got = null; globalThis.errs = [];
         const w = new Worker('worker.js'); w.onmessage = e => { got = e.data; }; w.onerror = e => errs.push(e.message);",
    );
    wpump(&mut r, 400);
    assert_eq!(s(&mut r, "errs"), "[]");
    for expect in [
        "document:",
        "window:",
        "localStorage:",
        "rAF:",
        "nested:NotSupportedError",
        "importScripts:",
    ] {
        assert!(
            s(&mut r, "got").contains(expect),
            "missing '{expect}': {}",
            s(&mut r, "got")
        );
    }
    assert!(
        s(&mut r, "got").contains("not available in dedicated workers"),
        "{}",
        s(&mut r, "got")
    );
    run(&mut r, "w.terminate();");
    wpump(&mut r, 150);
}

#[test]
fn worker_fetch_and_worker_timers() {
    let mut r = wrt(&[(
        "worker.js",
        "postMessage('started');
         setTimeout(() => {
           fetch('data:text/plain,hi-worker').then(r => r.text()).then(t => postMessage({ fetched: t }));
         }, 5);
         fetch('data:application/json;base64,eyJqIjoxfQ==').then(r => r.json()).then(j => postMessage({ json: j }));",
    )]);
    run(
        &mut r,
        "globalThis.got = []; const w = new Worker('worker.js'); w.onmessage = e => got.push(e.data);",
    );
    wpump(&mut r, 500);
    assert!(s(&mut r, "got").contains("started"), "{}", s(&mut r, "got"));
    assert!(
        s(&mut r, "got").contains(r#"{"fetched":"hi-worker"}"#),
        "data: URL fetch in worker: {}",
        s(&mut r, "got")
    );
    assert!(
        s(&mut r, "got").contains(r#"{"json":{"j":1}}"#),
        "{}",
        s(&mut r, "got")
    );
    run(&mut r, "w.terminate();");
    wpump(&mut r, 150);
}

#[test]
fn worker_idle_pump_cost_is_negligible_with_no_workers() {
    let mut r = rt("");
    // No Worker objects created: the prelude outbox drain + empty channel probe dominate.
    // This is a smoke bound (real cost is ~µs); it guards against accidental blocking.
    let start = std::time::Instant::now();
    for _ in 0..100 {
        assert!(r.poll_workers().is_empty());
    }
    let elapsed = start.elapsed();
    assert!(!r.worker_alive() && !r.worker_pending());
    assert!(
        elapsed < std::time::Duration::from_secs(2),
        "100 idle pumps took {elapsed:?}"
    );
}

#[test]
fn worker_console_forwards_with_prefix() {
    let mut r = wrt(&[(
        "worker.js",
        "console.log('hello from worker', 42); postMessage('logged');",
    )]);
    run(
        &mut r,
        "globalThis.got = null; const w = new Worker('worker.js'); w.onmessage = e => { got = e.data; };",
    );
    wpump(&mut r, 300);
    assert_eq!(s(&mut r, "got"), "logged");
    let lines: Vec<String> = r
        .host
        .borrow_mut()
        .take_console()
        .into_iter()
        .map(|(l, m)| format!("{l}:{m}"))
        .collect();
    assert!(
        lines
            .iter()
            .any(|l| l.contains("[worker 1] hello from worker 42")),
        "worker console forwarded: {lines:?}"
    );
    run(&mut r, "w.terminate();");
    wpump(&mut r, 150);
}

// ------------------------------------------------------------------------------------
// Canvas 2D (A3, approved subset)
// ------------------------------------------------------------------------------------

#[test]
fn canvas_2d_context_kinds_and_attributes() {
    let mut r = rt("<canvas id=c width=20 height=10></canvas>");
    run(
        &mut r,
        "globalThis.c = document.getElementById('c'); globalThis.x = c.getContext('2d');",
    );
    assert_eq!(s(&mut r, "x !== null && typeof x.fillRect"), "function");
    assert_eq!(
        s(&mut r, "c.getContext('2d') === x"),
        "true",
        "one context object per canvas"
    );
    assert_eq!(
        s(&mut r, "[c.getContext('2d') === x, c.getContext('webgl2') === c.getContext('webgl2'), c.getContext('webgl') !== null]"),
        "[true,true,true]",
        "one context object per kind, cached"
    );
    assert_eq!(s(&mut r, "[c.getContext('webgpu'), c.getContext('bitmaprenderer'), c.getContext('nope')]"), "[null,null,null]");
    assert_eq!(s(&mut r, "[c.width, c.height]"), "[20,10]");
    assert_eq!(
        s(
            &mut r,
            "[typeof CanvasRenderingContext2D, typeof CanvasGradient, typeof ImageData]"
        ),
        r#"["function","function","function"]"#
    );
    assert_eq!(
        s(&mut r, "c.toDataURL().slice(0, 22)"),
        "data:image/png;base64,"
    );
    // width/height reflect attributes; setting width clears the bitmap and resets state.
    run(
        &mut r,
        "x.fillStyle = 'red'; x.fillRect(0, 0, 20, 10); c.width = 20;",
    );
    assert_eq!(
        s(&mut r, "[...x.getImageData(0, 0, 1, 1).data].join()"),
        "0,0,0,0"
    );
    assert_eq!(s(&mut r, "[c.width, x.fillStyle]"), "[20,\"#000000\"]");
}

#[test]
fn canvas_to_blob_and_composite_operations() {
    let mut r = rt("<canvas id=c width=8 height=8></canvas>");
    run(&mut r, "globalThis.c = document.getElementById('c'); globalThis.x = c.getContext('2d');
      globalThis.blob = null; c.toBlob(b => { blob = b; });
      globalThis.blobInfo = [blob !== null, blob.type, blob.size > 0];");
    assert_eq!(s(&mut r, "blobInfo"), r#"[true,"image/png",true]"#);
    // multiply: red then blue gives black, while source-over would give blue.
    run(&mut r, "x.globalCompositeOperation = 'multiply'; x.fillStyle = 'red'; x.fillRect(0,0,8,8); x.fillStyle = 'blue'; x.fillRect(0,0,8,8);");
    assert_eq!(s(&mut r, "x.globalCompositeOperation"), "multiply");
    assert_eq!(s(&mut r, "[...x.getImageData(4,4,1,1).data].join()"), "0,0,0,255");
    // copy replaces the destination including alpha.
    run(&mut r, "x.globalCompositeOperation = 'copy'; x.fillStyle = 'rgba(0,255,0,0.5)'; x.fillRect(0,0,8,8);");
    assert_eq!(s(&mut r, "[...x.getImageData(4,4,1,1).data].join()"), "0,255,0,128");
    // Non-separable modes are accepted but fall back to source-over.
    run(&mut r, "x.globalCompositeOperation = 'hue';");
    assert_eq!(s(&mut r, "x.globalCompositeOperation"), "source-over");
}

#[test]
fn select_show_picker_opens_a_popup_and_commits_a_choice() {
    let mut r = rt("<select id=s><option>A</option><option>B</option><option>C</option></select>");
    run(
        &mut r,
        "globalThis.changes = 0; const s = document.getElementById('s');
         s.addEventListener('change', () => changes++); s.showPicker();
         globalThis.popup = document.body.lastElementChild;
         globalThis.items = Array.from(popup.children).map(c => c.textContent);",
    );
    assert_eq!(s(&mut r, "items"), r#"["A","B","C"]"#);
    run(&mut r, "popup.children[2].onclick();");
    assert_eq!(s(&mut r, "s.value"), "C");
    assert_eq!(s(&mut r, "changes"), "1");
}

#[test]
fn date_input_show_picker_opens_a_calendar() {
    let mut r = rt("<input id=d type=date value=2024-05-10>");
    run(
        &mut r,
        "const d = document.getElementById('d'); d.showPicker();
         globalThis.popup = document.body.lastElementChild;
         globalThis.title = popup.children[0].textContent;",
    );
    assert_eq!(s(&mut r, "title"), "2024-05");
}

#[test]
fn file_input_show_picker_uses_the_os_dialog() {
    let mut r = rt("<input id=f type=file accept=.png,.jpg multiple>");
    r.host.borrow_mut().set_dialog_mock(Some(Some(vec!["/pics/a.png".into(), "/pics/b.png".into()])));
    run(
        &mut r,
        "globalThis.changes = 0; const f = document.getElementById('f');
         f.addEventListener('change', () => changes++); f.showPicker();",
    );
    pump(&mut r, 40);
    assert_eq!(s(&mut r, "f.files.length"), "2");
    assert_eq!(s(&mut r, "f.files[0].name"), "a.png");
    assert_eq!(s(&mut r, "f.files[1].path"), "/pics/b.png");
    assert_eq!(s(&mut r, "changes"), "1");
}

#[test]
fn offscreen_canvas_draws_and_converts_to_blob() {
    let mut r = rt("");
    run(
        &mut r,
        "globalThis.off = new OffscreenCanvas(6, 6); globalThis.ox = off.getContext('2d');
         ox.fillStyle = 'lime'; ox.fillRect(0, 0, 6, 6);
         globalThis.px = [...ox.getImageData(2, 2, 1, 1).data].join();
         off.convertToBlob().then(b => { globalThis.blob = [b.type, b.size > 0]; });",
    );
    assert_eq!(s(&mut r, "px"), "0,255,0,255");
    assert_eq!(s(&mut r, "[off.width, off.height]"), "[6,6]");
    // Let the blob promise resolve.
    pump(&mut r, 30);
    assert_eq!(s(&mut r, "blob"), r#"["image/png",true]"#);
}

#[test]
fn canvas_2d_pixels_paths_gradients_and_images() {
    let mut r =
        rt("<canvas id=c width=24 height=24></canvas><canvas id=d width=8 height=8></canvas>");
    run(
        &mut r,
        "globalThis.c = document.getElementById('c'); globalThis.x = c.getContext('2d');
         x.fillStyle = 'red'; x.fillRect(0, 0, 24, 24);
         globalThis.px = (a, b) => [...x.getImageData(a, b, 1, 1).data].join();
         globalThis.red = px(1, 1);",
    );
    assert_eq!(s(&mut r, "red"), "255,0,0,255");
    // Paths: triangle fill.
    run(&mut r, "x.fillStyle = '#0000ff'; x.beginPath(); x.moveTo(2, 20); x.lineTo(22, 20); x.lineTo(12, 4); x.closePath(); x.fill();");
    assert_eq!(s(&mut r, "px(12, 16)"), "0,0,255,255");
    // Arc + stroke with round caps.
    run(&mut r, "x.strokeStyle = 'black'; x.lineWidth = 4; x.lineCap = 'round'; x.beginPath(); x.moveTo(2, 2); x.lineTo(10, 2); x.stroke();");
    assert_eq!(s(&mut r, "px(6, 2)"), "0,0,0,255");
    // Linear gradient (2-stop) across a fresh rect.
    run(
        &mut r,
        "x.clearRect(0, 0, 24, 24); const g = x.createLinearGradient(0, 0, 24, 0); g.addColorStop(0, 'black'); g.addColorStop(1, 'white');
         x.fillStyle = g; x.fillRect(0, 0, 24, 4);
         globalThis.gok = (() => { const [a, b, c] = [px(1, 2), px(12, 2), px(22, 2)].map(s => +s.split(',')[0]); return a < 40 && Math.abs(b - 128) < 20 && c > 210; })();",
    );
    assert_eq!(s(&mut r, "gok"), "true");
    // Transform: scale moves paint.
    run(&mut r, "x.clearRect(0, 0, 24, 24); x.save(); x.setTransform(2, 0, 0, 2, 0, 0); x.fillStyle = 'lime'; x.fillRect(0, 0, 4, 4); x.restore();");
    assert_eq!(
        s(&mut r, "[px(1, 1), px(5, 5), px(8, 8)].join('|')"),
        "0,255,0,255|0,255,0,255|0,0,0,0"
    );
    // drawImage canvas -> canvas, then get/put round-trip.
    run(
        &mut r,
        "const d = document.getElementById('d'); const dx = d.getContext('2d');
         dx.drawImage(c, 4, 4, 4, 4, 0, 0, 8, 8);
         globalThis.copied = [...dx.getImageData(0, 0, 1, 1).data].join();
         const snap = x.getImageData(0, 0, 24, 24); x.clearRect(0, 0, 24, 24); x.putImageData(snap, 0, 0);
         globalThis.restored = px(1, 1);",
    );
    assert_eq!(s(&mut r, "copied"), "0,255,0,255");
    assert_eq!(s(&mut r, "restored"), "0,255,0,255");
    // Invalid styles are ignored (restore() puts the saved gradient back, so park a
    // plain color first to keep the assertion readable). Bad arcs throw IndexSizeError,
    // zero-size reads throw. Radial gradients now paint (negative radii still throw);
    // only patterns stay unsupported.
    run(
        &mut r,
        "globalThis.errs = []; x.fillStyle = 'lime'; x.fillStyle = 'nope-not-a-color'; globalThis.kept = x.fillStyle;
         try { x.arc(1, 1, -2, 0, 1); } catch (e) { errs.push(e.name); }
         try { x.getImageData(0, 0, 0, 4); } catch (e) { errs.push(e.name); }
         try { x.createRadialGradient(0,0,-1,0,0,2); } catch (e) { errs.push(e.name); }
         try { x.createPattern(null); } catch (e) { errs.push(e.name); }
         globalThis.radialOk = (() => { try { x.createRadialGradient(0,0,1,0,0,2); x.createConicGradient(0,0,0); return true; } catch (_) { return false; } })();",
    );
    assert_eq!(
        s(&mut r, "[kept, errs, radialOk]"),
        r#"["lime",["IndexSizeError","IndexSizeError","IndexSizeError","NotSupportedError"],true]"#
    );
}

#[test]
fn canvas_2d_remainder_text_path2d_clip_gradients_shadows() {
    let mut r = rt("<canvas id=c width=64 height=64></canvas>");
    run(
        &mut r,
        "globalThis.c = document.getElementById('c'); globalThis.x = c.getContext('2d');
         globalThis.px = (a, b) => [...x.getImageData(a, b, 1, 1).data].join();",
    );
    // Radial gradient: white center, black edge.
    run(
        &mut r,
        "x.clearRect(0, 0, 64, 64);
         const rg = x.createRadialGradient(32, 32, 2, 32, 32, 30);
         rg.addColorStop(0, 'white'); rg.addColorStop(1, 'black');
         x.fillStyle = rg; x.fillRect(0, 0, 64, 64);
         globalThis.radial = [px(32, 32), px(0, 0)];",
    );
    assert_eq!(s(&mut r, "radial"), r#"["255,255,255,255","0,0,0,255"]"#);
    // Conic gradient: the 12-o'clock ray holds the first stop, the sweep blends.
    // (Pixel centers sit half a pixel off the ray, so the assertions allow the
    // sub-percent blend that produces.)
    run(
        &mut r,
        "x.clearRect(0, 0, 64, 64);
         const cg = x.createConicGradient(0, 32, 32);
         cg.addColorStop(0, 'red'); cg.addColorStop(1, 'blue');
         x.fillStyle = cg; x.fillRect(0, 0, 64, 64);
         globalThis.conic = (() => { const [a, b] = [px(32, 48), px(48, 32)].map(s => s.split(',').map(Number)); return [a[0] > 200 && a[2] < 60, a.join() !== b.join()]; })();",
    );
    assert_eq!(s(&mut r, "conic"), "[true,true]");
    // Gradient stop validation still throws SyntaxError for out-of-range offsets.
    run(
        &mut r,
        "globalThis.gradErr = (() => { try { const g = x.createLinearGradient(0,0,1,1); g.addColorStop(2, 'red'); } catch (e) { return e.name; } return 'no-throw'; })();",
    );
    assert_eq!(s(&mut r, "gradErr"), "SyntaxError");
    // Path2D: own path object usable in fill/stroke, copy constructor, evenodd holes.
    run(
        &mut r,
        "x.clearRect(0, 0, 64, 64); x.save();
         const p = new Path2D(); p.rect(40, 2, 10, 10);
         x.fillStyle = 'red'; x.fill(p);
         globalThis.pathFill = px(45, 7);
         const q = new Path2D(p); x.fillStyle = 'blue'; x.fill(q);
         globalThis.pathCopy = px(45, 7);
         const s2 = new Path2D(); s2.moveTo(2, 50); s2.lineTo(20, 50);
         x.strokeStyle = 'black'; x.lineWidth = 4; x.stroke(s2);
         globalThis.pathStroke = px(10, 50);
         const e = new Path2D(); e.rect(0, 0, 20, 20); e.rect(5, 5, 10, 10);
         x.fillStyle = 'magenta'; x.fill(e, 'evenodd');
         globalThis.evenodd = [px(2, 2), px(10, 10)];
         globalThis.pathStrErr = (() => { try { new Path2D('M0 0h10v10H0z'); } catch (err) { return err.name; } return 'no-throw'; })();
         x.restore();",
    );
    assert_eq!(s(&mut r, "pathFill"), "255,0,0,255");
    assert_eq!(s(&mut r, "pathCopy"), "0,0,255,255");
    assert_eq!(s(&mut r, "pathStroke"), "0,0,0,255");
    assert_eq!(
        s(&mut r, "evenodd"),
        r#"["255,0,255,255","0,0,0,0"]"#,
        "evenodd leaves the inner rect unpainted"
    );
    assert_eq!(s(&mut r, "pathStrErr"), "NotSupportedError");
    assert_eq!(s(&mut r, "typeof Path2D"), "function");
    // clip(): intersect, restore-on-save, Path2D form.
    run(
        &mut r,
        "x.clearRect(0, 0, 64, 64); x.save();
         x.beginPath(); x.rect(0, 0, 8, 8); x.clip();
         x.fillStyle = 'lime'; x.fillRect(0, 0, 64, 64);
         globalThis.clipIn = px(4, 4); globalThis.clipOut = px(20, 20);
         const cp = new Path2D(); cp.rect(40, 40, 8, 8); x.clip(cp);
         x.fillRect(0, 0, 64, 64);
         globalThis.clipDisjoint = px(4, 4);
         x.restore(); x.fillStyle = 'lime'; x.fillRect(0, 0, 64, 64);
         globalThis.clipRestored = px(20, 20);",
    );
    assert_eq!(
        s(&mut r, "[clipIn, clipOut, clipDisjoint, clipRestored]"),
        r#"["0,255,0,255","0,0,0,0","0,255,0,255","0,255,0,255"]"#
    );
    // Text: font wiring, measureText, fillText pixels, alignment, invalid fonts ignored.
    run(
        &mut r,
        "x.clearRect(0, 0, 64, 64);
         x.font = '20px sans-serif'; globalThis.fontAfterGood = x.font;
         x.font = 'bogus'; globalThis.fontKept = x.font;
         const m = x.measureText('Hello');
         globalThis.metrics = [m.width > 0, m.actualBoundingBoxAscent > 0];
         x.fillStyle = 'black'; x.textBaseline = 'alphabetic'; x.textAlign = 'start';
         x.fillText('Hi', 2, 30);
         let found = 0; for (let yy = 10; yy < 36 && !found; yy++) for (let xx = 0; xx < 40 && !found; xx++) if (+px(xx, yy).split(',')[3] > 0) found = 1;
         globalThis.textFound = found;
         x.clearRect(0, 0, 64, 64); x.textAlign = 'right'; x.fillText('Hi', 64, 30);
         let right = 0; for (let xx = 40; xx < 64 && !right; xx++) for (let yy = 10; yy < 36 && !right; yy++) if (+px(xx, yy).split(',')[3] > 0) right = 1;
         let left = 0; for (let xx = 0; xx < 20 && !left; xx++) for (let yy = 10; yy < 36 && !left; yy++) if (+px(xx, yy).split(',')[3] > 0) left = 1;
         globalThis.alignRight = [right, left];
         x.textAlign = 'start';
         globalThis.maxW = x.measureText('Hello world, this is long').width > x.measureText('Hi').width;
         x.fillText('Hello', 2, 50, 4);
         globalThis.strokeTextOk = (() => { x.strokeStyle = 'black'; x.strokeText('S', 30, 20); return true; })();",
    );
    assert_eq!(s(&mut r, "fontAfterGood"), "20px sans-serif");
    assert_eq!(s(&mut r, "fontKept"), "20px sans-serif", "invalid fonts are ignored");
    assert_eq!(s(&mut r, "metrics"), "[true,true]");
    assert_eq!(s(&mut r, "textFound"), "1", "fillText leaves opaque pixels");
    assert_eq!(s(&mut r, "alignRight"), "[1,0]", "right alignment hugs the right edge");
    assert_eq!(s(&mut r, "maxW"), "true");
    assert_eq!(s(&mut r, "strokeTextOk"), "true");
    // save/restore round-trips the font cache too.
    run(
        &mut r,
        "x.font = '20px serif'; x.save(); x.font = '10px monospace'; x.restore(); globalThis.fontRestored = x.font;",
    );
    assert_eq!(s(&mut r, "fontRestored"), "20px serif");
    // Shadows: offset shadow beside the shape, blur spreads it, save/restore works.
    run(
        &mut r,
        "x.clearRect(0, 0, 64, 64);
         x.save(); x.shadowColor = 'red'; x.shadowBlur = 0; x.shadowOffsetX = 10; x.shadowOffsetY = 0;
         x.fillStyle = 'blue'; x.fillRect(2, 2, 4, 4);
         globalThis.shadow = [px(3, 4), px(13, 4), px(30, 4)];
         x.shadowBlur = 6; x.shadowOffsetX = 0; x.fillRect(30, 2, 4, 4);
         globalThis.shadowBlur = px(27, 4);
         x.restore(); globalThis.shadowAfter = [x.shadowColor, x.shadowBlur, x.shadowOffsetX];
         x.fillStyle = 'blue'; x.fillRect(50, 50, 4, 4);
         globalThis.noShadow = [px(52, 52), px(62, 52)];",
    );
    assert_eq!(
        s(&mut r, "shadow"),
        r#"["0,0,255,255","255,0,0,255","0,0,0,0"]"#,
        "shape on top, offset shadow beside it"
    );
    assert_ne!(
        s(&mut r, "shadowBlur"),
        "0,0,0,0",
        "blur spreads the shadow past the shape"
    );
    assert_eq!(
        s(&mut r, "shadowAfter"),
        r#"["rgba(0, 0, 0, 0)",0,0]"#,
        "restore drops shadow state"
    );
    assert_eq!(s(&mut r, "noShadow"), r#"["0,0,255,255","0,0,0,0"]"#);
    // Composite ops now apply; filter is still unsupported.
    run(
        &mut r,
        "x.globalCompositeOperation = 'lighter'; globalThis.gco = x.globalCompositeOperation;
         globalThis.unsupported = [typeof c.toBlob, typeof OffscreenCanvas, typeof x.filter];",
    );
    assert_eq!(
        s(&mut r, "[gco, unsupported]"),
        r#"["lighter",["function","function","undefined"]]"#
    );
}

#[test]
fn svg_image_decodes_and_draws_to_canvas_with_getimagedata_pixels() {
    // `<img src=.svg>` decodes through the same engine path as PNG: natural size from
    // width/height (or viewBox), CPU raster upload, `drawImage` + `getImageData` pixels.
    let mut assets = MapAssets::default();
    assets.0.insert(
        "r.svg".into(),
        br##"<svg width="40" height="20"><rect x="0" y="0" width="40" height="20" fill="#ff0000"/><circle cx="30" cy="10" r="6" fill="rgb(0,0,255)"/></svg>"##.to_vec(),
    );
    let mut r = rt_with("<canvas id=c width=40 height=20></canvas>", assets);
    run(
        &mut r,
        "globalThis.c = document.getElementById('c'); globalThis.x = c.getContext('2d');
         const img = new Image(); img.src = 'r.svg';
         globalThis.nat = [img.naturalWidth, img.naturalHeight, img.complete];
         x.drawImage(img, 0, 0);
         globalThis.px = (a, b) => [...x.getImageData(a, b, 1, 1).data].join();",
    );
    assert_eq!(s(&mut r, "nat"), "[40,20,true]");
    assert_eq!(s(&mut r, "px(2, 10)"), "255,0,0,255");
    // Circle centre (30,10) paints blue over the red rect.
    assert_eq!(s(&mut r, "px(30, 10)"), "0,0,255,255");
}

#[test]
fn canvas_draws_do_not_restlye_or_relayout_siblings() {
    let mut r = rt("<style>#a{width:40px;height:40px;background:green}</style><div id=a></div><canvas id=c width=16 height=16></canvas><div id=b></div>");
    r.engine.borrow_mut().update(0.0);
    let (rs0, lo0) = {
        let e = r.engine.borrow();
        (e.stats.restyles, e.stats.layouts)
    };
    run(&mut r, "const x = document.getElementById('c').getContext('2d'); x.fillStyle = 'red'; x.fillRect(0, 0, 16, 16);");
    let info = r.engine.borrow_mut().update(0.0);
    let e = r.engine.borrow();
    assert!(info.repainted, "a canvas draw repaints");
    assert_eq!(
        (e.stats.restyles, e.stats.layouts),
        (rs0, lo0),
        "no sibling restyle or relayout"
    );
}

// ------------------------------------------------------------------------------------
// 3D scenes (v1 subset): primary RendrixScene API, webgl2-context alias, CPU rasterizer
// ------------------------------------------------------------------------------------

#[test]
fn scene3d_api_presence_and_aliases() {
    let mut r = rt("<canvas id=c width=20 height=10></canvas><rendrix-scene id=s></rendrix-scene>");
    run(
        &mut r,
        "globalThis.c = document.getElementById('c');
         globalThis.gl2 = c.getContext('webgl2');
         globalThis.gl1 = c.getContext('webgl');
         globalThis.box = Rendrix3D.createBoxScene({ color: [1, 0, 0, 1] });",
    );
    assert_eq!(
        s(&mut r, "[gl2 !== null, gl1 !== null, gl2 instanceof WebGL2RenderingContext, gl1 instanceof WebGLRenderingContext, gl2 === c.getContext('webgl2')]"),
        "[true,true,true,true,true]"
    );
    assert_eq!(
        s(&mut r, "[box.meshes.length, box.meshes[0].geo.kind, box.camera.position.join()]"),
        r#"[1,"box","0,0,6"]"#
    );
    // RendrixScene hosts: canvas and <rendrix-scene> both attach, round-trip and remove.
    run(
        &mut r,
        "globalThis.h1 = RendrixScene.createScene('c', box);
         globalThis.h2 = RendrixScene.createScene(document.getElementById('s'), box);
         globalThis.roundtrip = [RendrixScene.getScene('c').meshes.length, RendrixScene.getScene('s').camera.fovY];
         h1.setCamera({ fovY: 60 }); h1.addBox({ size: 1, color: [0, 1, 0, 1] });
         globalThis.after = RendrixScene.getScene('c').meshes.length;
         h1.remove(); globalThis.gone = RendrixScene.getScene('c');",
    );
    assert_eq!(s(&mut r, "roundtrip"), r#"[1,40]"#);
    assert_eq!(s(&mut r, "after"), "2", "setCamera keeps meshes, addBox appends");
    assert_eq!(s(&mut r, "gone"), "null", "remove drops the scene");
    assert_eq!(
        s(&mut r, "RendrixScene.getScene('missing-id')"),
        "null",
        "unknown elements read back as null (no throw)"
    );
}

#[test]
fn scene3d_validation_errors_and_unsupported_gl_calls() {
    let mut r = rt("<canvas id=c width=20 height=10></canvas>");
    run(
        &mut r,
        "globalThis.errs = [];
         try { RendrixScene.createScene('c', { meshes: [{ geo: { kind: 'torus' } }] }); } catch (e) { errs.push(e.name); }
         try { RendrixScene.createScene('c', { meshes: [{ geo: { kind: 'tris', verts: [0, 1] } }] }); } catch (e) { errs.push(e.name); }
         try { RendrixScene.createScene(document.body, {}); } catch (e) { errs.push(e.name); }
         try { RendrixScene.createScene('no-such-id', {}); } catch (e) { errs.push(e.constructor.name); }
         try { RendrixScene.createScene('c', 'not-an-object'); } catch (e) { errs.push(e.constructor.name); }",
    );
    assert_eq!(
        s(&mut r, "errs"),
        r#"["NotSupportedError","NotSupportedError","TypeError","TypeError","TypeError"]"#,
        "bad descs and bad hosts fail loudly, never silently"
    );
    // Unsupported GL calls warn and no-op (or return null) — never crash.
    run(
        &mut r,
        "const gl = document.getElementById('c').getContext('webgl2');
         globalThis.warns = []; const origWarn = console.warn; console.warn = m => warns.push(String(m));
         let tex = null, threw = null;
         try {
           tex = gl.createTexture();
           gl.drawElements(gl.TRIANGLES, 0, 0);
           gl.enable(12345);
           gl.drawArrays(5, 0, 3);
           gl.drawArrays(gl.TRIANGLES, 0, 3);
           globalThis.params = [gl.getError(), gl.isContextLost(), gl.getParameter(99999), gl.getExtension('WEBGL_debug')];
         } catch (e) { threw = String(e); }
         console.warn = origWarn;",
    );
    assert_eq!(s(&mut r, "threw"), "null");
    assert_eq!(s(&mut r, "tex"), "null");
    assert_eq!(s(&mut r, "params"), "[0,false,null,null]");
    assert_eq!(s(&mut r, "warns.length >= 5"), "true", "every unsupported call warned");
}

#[test]
fn scene3d_renders_into_the_canvas_bitmap_only_when_dirty() {
    let mut r = rt("<canvas id=c width=200 height=150></canvas>");
    run(
        &mut r,
        "RendrixScene.createScene('c', Rendrix3D.createBoxScene({ color: [1, 0, 0, 1], camPos: [0, 0, 6] }));",
    );
    let px = |rgba: &[u8], w: u32, x: u32, y: u32| {
        let i = ((y * w + x) * 4) as usize;
        [rgba[i], rgba[i + 1], rgba[i + 2], rgba[i + 3]]
    };
    let first = crate::canvas_ops::take_canvas_uploads(&r.engine.borrow());
    assert_eq!(first.len(), 1, "one upload for the dirty scene canvas");
    assert_eq!((first[0].width, first[0].height), (200, 150));
    let center = px(&first[0].rgba, 200, 100, 75);
    assert!(
        center[0] > 100 && center[1] < 60 && center[2] < 60 && center[3] == 255,
        "red box face covers the canvas center: {center:?}"
    );
    assert_eq!(px(&first[0].rgba, 200, 2, 2)[3], 0, "corners stay transparent");
    // No changes → no upload (per-frame upload only when dirty).
    let second = crate::canvas_ops::take_canvas_uploads(&r.engine.borrow());
    assert!(second.is_empty(), "clean scenes upload nothing");
    // A triangle-soup draw through the webgl2 alias lands in the same bitmap.
    run(
        &mut r,
        "const gl = document.getElementById('c').getContext('webgl2');
         gl.clearColor(0, 0, 0, 1); gl.clear(gl.COLOR_BUFFER_BIT);
         gl.bufferData(gl.ARRAY_BUFFER, new Float32Array([-1,-1,-1, 3,-1,-1, -1,3,-1]), gl.STATIC_DRAW);
         gl.uniform4f(null, 1, 1, 1, 1);
         gl.drawArrays(gl.TRIANGLES, 0, 3);",
    );
    let third = crate::canvas_ops::take_canvas_uploads(&r.engine.borrow());
    assert_eq!(third.len(), 1);
    let lit = px(&third[0].rgba, 200, 100, 75);
    assert!(lit[0] > 100 && lit[3] == 255, "white soup triangle covers the center: {lit:?}");
}

#[test]
fn scene3d_element_removal_drops_the_scene() {
    let mut r = rt("<canvas id=c width=40 height=30></canvas>");
    run(
        &mut r,
        "RendrixScene.createScene('c', Rendrix3D.createBoxScene({}));
         globalThis.keep = document.getElementById('c');",
    );
    let id = r.engine.borrow().doc.get_element_by_id("c").unwrap();
    let get = |r: &mut JsRuntime| {
        r.host
            .borrow_mut()
            .call("scene3d_get", &[Val::Num(id as f64)])
    };
    assert!(matches!(get(&mut r), Ok(Val::Str(_))), "scene is stored");
    // Detach and drop every JS reference, then sweep like the frame loop does. The
    // strong-wrapper reconcile is throttled (500 ms), so wait it out.
    run(&mut r, "document.body.innerHTML = ''; globalThis.keep = null;");
    std::thread::sleep(std::time::Duration::from_millis(700));
    run(&mut r, "1;");
    r.collect_garbage();
    run(&mut r, "1;");
    r.collect_garbage();
    run(&mut r, "1;");
    r.engine.borrow_mut().doc.collect_garbage();
    r.host.borrow_mut().call("take_freed", &[]).unwrap();
    assert!(
        matches!(get(&mut r), Ok(Val::Null)),
        "the scene died with its element"
    );
}

#[test]
fn scene3d_js_scenes_reach_the_gpu_store() {
    // The JS scene API bridges every upsert into the gfx scene store, which drives the
    // GPU viewport for <rendrix-scene> / <canvas data-3d>. A schema mismatch used to be
    // silently discarded by upsert(), leaving the GPU path empty. This test pins the
    // bridge end to end: JS desc -> canonical JSON -> gfx Scene::from_json -> store.
    let mut r = rt("<canvas id=c data-3d width=20 height=10></canvas><rendrix-scene id=s></rendrix-scene>");
    let (canvas_id, host_id) = {
        let e = r.engine.borrow();
        (e.doc.get_element_by_id("c").unwrap(), e.doc.get_element_by_id("s").unwrap())
    };
    rendrix_gfx::scene3d::remove_scene(canvas_id as u64);
    rendrix_gfx::scene3d::remove_scene(host_id as u64);

    run(
        &mut r,
        "globalThis.hc = RendrixScene.createScene('c', Rendrix3D.createBoxScene({ color: [1,0,0,1] }));
         globalThis.hs = RendrixScene.createScene(document.getElementById('s'), Rendrix3D.createBoxScene({ color: [0,1,0,1] }));",
    );
    let ids = rendrix_gfx::scene3d::scene_ids();
    assert!(ids.contains(&(canvas_id as u64)), "canvas data-3d host must reach the GPU store: {ids:?}");
    assert!(ids.contains(&(host_id as u64)), "<rendrix-scene> host must reach the GPU store: {ids:?}");

    // Updating the desc must refresh the GPU scene too, and removal must drop it.
    run(&mut r, "hc.setCamera({ fovY: 60 }); hs.remove();");
    let ids = rendrix_gfx::scene3d::scene_ids();
    assert!(ids.contains(&(canvas_id as u64)), "updated scene stays in the GPU store");
    assert!(!ids.contains(&(host_id as u64)), "removed scene leaves the GPU store");
    rendrix_gfx::scene3d::remove_scene(canvas_id as u64);
}
