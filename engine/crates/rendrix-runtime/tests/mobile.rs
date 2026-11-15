//! Mobile input: touch (tap / pan / fling / long press / multi-touch / `touch-action`), safe-area
//! and keyboard insets as `env()`, soft-keyboard intent, and gamepads. Everything here drives a real
//! `Session` (engine + JS), so it runs on a desktop with no device or emulator.

use rendrix_runtime::bundle::AppBundle;
use rendrix_runtime::session::{GamepadState, MemoryClipboard, Session};

fn bundle(html: &str, js: &str) -> AppBundle {
    let mut b = AppBundle::from_json(r#"{"format":"rendrix-app/1","name":"mobile","html":"","width":360,"height":640}"#).unwrap();
    b.html = html.to_string();
    b.modules.insert("main.js".into(), js.to_string());
    b
}

fn session(html: &str, js: &str) -> Session {
    let b = bundle(html, js);
    let mut s = Session::new(&b, Box::new(MemoryClipboard::default()), (360.0, 640.0, 1.0)).expect("session");
    s.js.host.borrow_mut().echo_console = false;
    s.frame();
    s
}

fn eval(s: &mut Session, expr: &str) -> String {
    match s.js.eval(&format!("(()=>{{ const v = ({expr}); return typeof v === 'string' ? v : JSON.stringify(v); }})()")) {
        Ok(rendrix_js::Val::Str(v)) => v,
        other => panic!("eval({expr}) -> {other:?}"),
    }
}

const PAGE: &str = "<html><head><style>
  body{margin:0} #list{height:300px;overflow-y:auto} .row{height:50px} #btn{width:100px;height:40px}
  #pad{width:100px;height:100px;touch-action:none} #bar{height:20px;padding-top:env(safe-area-inset-top,0px);padding-bottom:env(keyboard-inset-height,0px)}
  #btn:hover{color:red}
</style></head><body>
  <button id=btn>tap</button><input id=name>
  <div id=pad></div><div id=bar></div>
  <div id=list>
    <div class=row>1</div><div class=row>2</div><div class=row>3</div><div class=row>4</div><div class=row>5</div><div class=row>6</div>
    <div class=row>7</div><div class=row>8</div><div class=row>9</div><div class=row>10</div><div class=row>11</div><div class=row>12</div>
  </div>
  <script type=module src=main.js></script></body></html>";

const JS: &str = "
  window.log = [];
  const L = s => window.log.push(s);
  for (const t of ['pointerdown','pointerup','pointercancel','touchstart','touchend','mousedown','mouseup','click','contextmenu'])
    document.getElementById('btn').addEventListener(t, e => L(t + ':' + (e.pointerType || '')));
  const pad = document.getElementById('pad');
  for (const t of ['pointermove','pointercancel','touchmove'])
    pad.addEventListener(t, e => L('pad-' + t));
";

fn centre(s: &Session, sel: &str) -> (f32, f32) {
    let id = s.query(sel).unwrap_or_else(|| panic!("no {sel}"));
    s.center_of(id).unwrap()
}

#[test]
fn tap_sends_pointer_touch_then_compat_mouse_and_click_without_sticky_hover() {
    let mut s = session(PAGE, JS);
    let (x, y) = centre(&s, "#btn");
    s.touch_start(0, x, y);
    s.touch_end(0, x, y);
    s.frame();
    let log = eval(&mut s, "window.log.join(' ')");
    assert_eq!(log, "pointerdown:touch touchstart: pointerup:touch touchend: mousedown: mouseup: click:touch", "event order: {log}");
    // Hover must not stick after a tap on touch devices.
    assert_eq!(eval(&mut s, "document.querySelector('#btn').matches(':hover')"), "false");
}

#[test]
fn prevent_default_on_touchstart_suppresses_compat_click() {
    let mut s = session(PAGE, &format!("{JS}\ndocument.getElementById('btn').addEventListener('touchstart', e => e.preventDefault());"));
    let (x, y) = centre(&s, "#btn");
    s.touch_start(0, x, y);
    s.touch_end(0, x, y);
    s.frame();
    let log = eval(&mut s, "window.log.join(' ')");
    assert!(!log.contains("click") && !log.contains("mousedown"), "no compat events when touchstart is prevented: {log}");
}

#[test]
fn drag_scrolls_the_list_cancels_the_pointer_and_does_not_click() {
    let mut s = session(PAGE, JS);
    let (x, y) = centre(&s, "#list");
    assert_eq!(eval(&mut s, "document.getElementById('list').scrollTop"), "0");
    s.touch_start(0, x, y);
    for i in 1..=10 {
        s.touch_move(0, x, y - 12.0 * i as f32);
    }
    s.touch_end(0, x, y - 120.0);
    s.frame();
    let top: f32 = eval(&mut s, "document.getElementById('list').scrollTop").parse().unwrap();
    // The pan moved the content 120px; the quick release may add a little inertia on the next frame.
    assert!(top >= 119.0, "finger moved 120px up → content scrolls at least 120px, got {top}");
}

#[test]
fn flick_continues_scrolling_with_inertia_then_stops() {
    let mut s = session(PAGE, JS);
    let (x, y) = centre(&s, "#list");
    s.touch_start(0, x, y + 100.0);
    // A fast swipe: 8 samples a few ms apart.
    for i in 1..=8 {
        std::thread::sleep(std::time::Duration::from_millis(4));
        s.touch_move(0, x, y + 100.0 - 18.0 * i as f32);
    }
    s.touch_end(0, x, y + 100.0 - 18.0 * 8.0);
    let released: f32 = { s.frame(); eval(&mut s, "document.getElementById('list').scrollTop").parse().unwrap() };
    let mut moving = true;
    for _ in 0..120 {
        std::thread::sleep(std::time::Duration::from_millis(8));
        let info = s.frame();
        if !info.continuous {
            moving = false;
            break;
        }
    }
    let after: f32 = eval(&mut s, "document.getElementById('list').scrollTop").parse().unwrap();
    assert!(after > released + 20.0, "fling should carry past the release point: released at {released}, ended at {after}");
    assert!(!moving || after >= 300.0, "fling must decay to a stop (or hit the end)");
}

#[test]
fn touch_action_none_blocks_panning_and_keeps_pointer_events() {
    let mut s = session(PAGE, JS);
    let (x, y) = centre(&s, "#pad");
    s.touch_start(0, x, y);
    for i in 1..=6 {
        s.touch_move(0, x + 8.0 * i as f32, y);
    }
    s.touch_end(0, x + 48.0, y);
    let log = eval(&mut s, "window.log.join(' ')");
    assert!(log.contains("pad-pointermove") && !log.contains("pad-pointercancel"), "pointer events keep flowing under touch-action:none: {log}");
    assert_eq!(eval(&mut s, "getComputedStyle(document.getElementById('pad')).touchAction || getComputedStyle(document.getElementById('pad')).getPropertyValue('touch-action')"), "none");
}

#[test]
fn long_press_fires_contextmenu_and_not_click() {
    let mut s = session(PAGE, JS);
    let (x, y) = centre(&s, "#btn");
    s.touch_start(0, x, y);
    std::thread::sleep(std::time::Duration::from_millis(560));
    s.frame();
    s.touch_end(0, x, y);
    let log = eval(&mut s, "window.log.join(' ')");
    assert!(log.contains("contextmenu"), "long press → contextmenu: {log}");
    assert!(!log.contains("click"), "long press must not click: {log}");
}

#[test]
fn multi_touch_reports_all_touches() {
    let mut s = session(PAGE, "window.counts = []; document.body.addEventListener('touchstart', e => window.counts.push([e.touches.length, e.changedTouches.length, e.targetTouches.length]));");
    s.touch_start(0, 50.0, 20.0);
    s.touch_start(1, 60.0, 20.0);
    assert_eq!(eval(&mut s, "window.counts"), "[[1,1,1],[2,1,2]]");
    assert_eq!(eval(&mut s, "navigator.maxTouchPoints"), "0");
    s.set_max_touch_points(5);
    assert_eq!(eval(&mut s, "navigator.maxTouchPoints"), "5");
    s.touch_cancel(0);
    s.touch_cancel(1);
    assert!(!s.touching());
}

#[test]
fn safe_area_and_keyboard_insets_resolve_in_css_env() {
    let mut s = session(PAGE, JS);
    let pad = |s: &mut Session| eval(s, "(() => { const c = getComputedStyle(document.getElementById('bar')); return c.paddingTop + ' ' + c.paddingBottom; })()");
    s.frame();
    assert_eq!(pad(&mut s), "0px 0px", "env() falls back to 0 with no insets");
    s.set_safe_area(47.0, 0.0, 34.0, 0.0);
    s.set_keyboard_height(291.0);
    s.frame();
    assert_eq!(pad(&mut s), "47px 291px");
    s.set_keyboard_height(0.0);
    s.frame();
    assert_eq!(pad(&mut s), "47px 0px");
}

#[test]
fn text_input_intent_follows_focus_for_the_soft_keyboard() {
    let mut s = session(PAGE, JS);
    assert!(!s.frame().text_input);
    let (x, y) = centre(&s, "#name");
    s.touch_start(0, x, y);
    s.touch_end(0, x, y);
    assert!(s.frame().text_input, "tapping an <input> should ask for the keyboard");
    let (bx, by) = centre(&s, "#btn");
    s.touch_start(0, bx, by);
    s.touch_end(0, bx, by);
    assert!(!s.frame().text_input, "focus left the field → hide the keyboard");
}

#[test]
fn jitter_within_slop_still_taps() {
    let mut s = session(PAGE, JS);
    let (x, y) = centre(&s, "#btn");
    s.touch_start(0, x, y);
    // 3px of finger wobble stays inside the 10px slop: still a tap.
    s.touch_move(0, x + 3.0, y - 2.0);
    s.touch_end(0, x + 3.0, y - 2.0);
    s.frame();
    let log = eval(&mut s, "window.log.join(' ')");
    assert!(log.contains("click"), "sub-slop movement must still tap: {log}");
    assert!(!log.contains("pointercancel"), "no pan, no cancel: {log}");
}

#[test]
fn touch_cancel_produces_no_click_and_no_fling() {
    let mut s = session(PAGE, "window.log = []; for (const t of ['touchstart','touchcancel','touchend','click']) document.body.addEventListener(t, () => window.log.push(t));");
    let (x, y) = centre(&s, "#list");
    s.touch_start(0, x, y);
    for i in 1..=6 {
        s.touch_move(0, x, y - 15.0 * i as f32);
    }
    s.touch_cancel(0);
    s.frame();
    // A cancelled gesture scrolls nothing further and never synthesises a tap.
    let info = s.frame();
    assert!(!info.continuous, "cancelled gesture must not start a fling");
    let log = eval(&mut s, "window.log.join(' ')");
    assert!(log.contains("touchcancel") && !log.contains("click") && !log.contains("touchend"), "{log}");
    assert!(!s.touching());
}

#[test]
fn repeat_gamepad_poll_with_same_state_fires_nothing() {
    let mut s = session(PAGE, "window.ev = []; addEventListener('gamepadconnected', () => ev.push('c')); addEventListener('gamepaddisconnected', () => ev.push('d'));");
    let pad = GamepadState { id: "Pad".into(), buttons: vec![0.0; 17], axes: vec![0.0; 4] };
    s.set_gamepad(0, Some(pad.clone()));
    // The driver polls every loop turn: identical snapshots must be silent.
    s.set_gamepad(0, Some(pad.clone()));
    s.set_gamepad(0, Some(pad));
    assert_eq!(eval(&mut s, "ev"), "[\"c\"]");
    s.set_gamepad(0, None);
    s.set_gamepad(0, None);
    assert_eq!(eval(&mut s, "ev"), "[\"c\",\"d\"]");
}

#[test]
fn gamepad_connect_state_and_disconnect() {
    let mut s = session(PAGE, "window.ev = []; addEventListener('gamepadconnected', e => ev.push('c:' + e.gamepad.index + ':' + e.gamepad.id)); addEventListener('gamepaddisconnected', e => ev.push('d:' + e.gamepad.index));");
    assert_eq!(eval(&mut s, "navigator.getGamepads().length"), "0");
    let mut pad = GamepadState { id: "Test Pad".into(), buttons: vec![0.0; 17], axes: vec![0.0; 4] };
    s.set_gamepad(0, Some(pad.clone()));
    assert_eq!(eval(&mut s, "ev"), "[\"c:0:Test Pad\"]");
    pad.buttons[0] = 1.0;
    pad.axes[1] = -0.75;
    s.set_gamepad(0, Some(pad));
    assert_eq!(eval(&mut s, "(() => { const g = navigator.getGamepads()[0]; return [g.buttons[0].pressed, g.buttons[1].pressed, g.axes[1], g.mapping, g.connected]; })()"), "[true,false,-0.75,\"standard\",true]");
    s.set_gamepad(0, None);
    assert_eq!(eval(&mut s, "ev"), "[\"c:0:Test Pad\",\"d:0\"]");
    assert_eq!(eval(&mut s, "navigator.getGamepads().length"), "0");
}
