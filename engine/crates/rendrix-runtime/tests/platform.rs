//! SA4 application-platform integration tests: dialogs, drag-drop, WebSocket, FormData,
//! multi-window isolation, the accessibility snapshot, and the update client.
//!
//! Session-level tests need no GPU. Tests that present pixels (`Headless`) skip gracefully when
//! no adapter is available, following the existing `gpu_pixels_reflect_interaction` pattern.

use rendrix_runtime::bundle::AppBundle;
use rendrix_runtime::headless::Headless;
use rendrix_runtime::session::{MemoryClipboard, Session};
use std::time::Duration;

fn bundle(html: &str, main_js: &str) -> AppBundle {
    let mut b = AppBundle::from_json(r#"{"format":"rendrix-app/1","name":"it","html":"","width":400,"height":300}"#).unwrap();
    b.html = html.to_string();
    b.modules.insert("main.js".into(), main_js.to_string());
    b
}

fn session(html: &str, js: &str) -> Session {
    let mut s = Session::new(&bundle(html, js), Box::new(MemoryClipboard::default()), (400.0, 300.0, 1.0)).expect("session");
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

fn pump(s: &mut Session, frames: usize) {
    for _ in 0..frames {
        s.frame();
        std::thread::sleep(Duration::from_millis(2));
    }
}

/// Minimal blocking HTTP server for update/cookie tests: answers each connection from `respond`.
fn http_server(respond: impl Fn(&str) -> (Vec<u8>, Vec<u8>) + Send + 'static) -> (u16, std::thread::JoinHandle<()>) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let handle = std::thread::spawn(move || {
        use std::io::{Read, Write};
        for stream in listener.incoming() {
            let mut stream = match stream {
                Ok(s) => s,
                Err(_) => return,
            };
            let mut buf = [0u8; 8192];
            let n = stream.read(&mut buf).unwrap_or(0);
            let req = String::from_utf8_lossy(&buf[..n]).into_owned();
            let (head, body) = respond(&req);
            let mut out = head;
            out.extend_from_slice(&body);
            let _ = stream.write_all(&out);
            let _ = stream.flush();
        }
    });
    (port, handle)
}

#[test]
fn dialogs_resolve_from_the_mock_without_os_ui() {
    let mut s = session("<html><body></body></html>", "globalThis.picked = 'none'; globalThis.saved = 'none';");
    s.js.host.borrow_mut().set_dialog_mock(Some(Some(vec!["/tmp/a.txt".to_string(), "/tmp/b.txt".to_string()])));
    s.js.run_script("t", "rendrix.dialog.showOpenDialog({ multiple: true }).then(v => { globalThis.picked = v; });").unwrap();
    s.js.host.borrow_mut().set_dialog_mock(Some(None));
    s.js.run_script("t", "rendrix.dialog.showSaveDialog({}).then(v => { globalThis.saved = v; });").unwrap();
    pump(&mut s, 20);
    assert_eq!(eval(&mut s, "picked"), r#"["/tmp/a.txt","/tmp/b.txt"]"#);
    assert_eq!(eval(&mut s, "saved"), "null");
}

#[test]
fn file_drop_delivers_drag_events_with_paths() {
    let html = "<html><body><div id=zone style='width:200px;height:100px'></div><script type=module src=main.js></script></body></html>";
    let js = "globalThis.log = []; const z = document.getElementById('zone');
        for (const t of ['dragenter','dragover','drop']) z.addEventListener(t, e => log.push([t, e.dataTransfer.files.length, e.dataTransfer.files[0] && e.dataTransfer.files[0].name, e.dataTransfer.files[0] && e.dataTransfer.files[0].path]));";
    let Ok(mut h) = Headless::new(&bundle(html, js), (400, 300), 1.0) else { eprintln!("no GPU, skipping"); return };
    h.session.js.host.borrow_mut().echo_console = false;
    h.settle(Duration::from_secs(1));
    h.drop_files(100.0, 50.0, &["/tmp/report.csv"]);
    h.settle(Duration::from_secs(1));
    let log = h.eval("log").unwrap();
    assert!(log.contains("drop") && log.contains("report.csv") && log.contains("/tmp/report.csv"), "{log}");
}

#[test]
fn form_data_encodes_multipart_and_sets_content_type() {
    let mut s = session("<html><body></body></html>", "");
    s.js.run_script(
        "t",
        "const fd = new FormData(); fd.append('title', 'hi'); fd.append('n', 7);
         fd.append('file', new File(['abc'], 'a.txt', { type: 'text/plain' }));
         const req = new Request('data:x', { method: 'POST', body: fd });
         globalThis.ct = req.headers.get('content-type');
         req.text().then(t => { globalThis.body = t; });",
    )
    .unwrap();
    pump(&mut s, 10);
    let ct = eval(&mut s, "ct");
    assert!(ct.starts_with("multipart/form-data; boundary=----RendrixFormBoundary"), "{ct}");
    let body = eval(&mut s, "body");
    assert!(body.contains("name=\"title\"") && body.contains("hi"), "{body}");
    assert!(body.contains("name=\"n\"") && body.contains("7"), "{body}");
    assert!(body.contains("name=\"file\"; filename=\"a.txt\"") && body.contains("Content-Type: text/plain") && body.contains("abc"), "{body}");
}

#[test]
fn websocket_full_lifecycle_against_a_local_echo_server() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        // Select the client's subprotocol (if any) so the handshake completes.
        let mut ws = tungstenite::accept_hdr(stream, |req: &tungstenite::handshake::server::Request, mut res: tungstenite::handshake::server::Response| {
            if let Some(p) = req.headers().get("Sec-WebSocket-Protocol") {
                res.headers_mut().insert("Sec-WebSocket-Protocol", p.clone());
            }
            Ok(res)
        })
        .unwrap();
        loop {
            match ws.read() {
                Ok(tungstenite::Message::Text(t)) => {
                    if ws.send(tungstenite::Message::Text(t)).is_err() {
                        break;
                    }
                }
                Ok(tungstenite::Message::Binary(b)) => {
                    if ws.send(tungstenite::Message::Binary(b)).is_err() {
                        break;
                    }
                }
                Ok(tungstenite::Message::Close(_)) => break,
                Err(_) => break,
                _ => {}
            }
        }
    });
    let mut s = session("<html><body></body></html>", "");
    s.js
        .run_script(
            "t",
            &format!(
                "globalThis.log = []; const ws = new WebSocket('ws://127.0.0.1:{port}/', ['echo']);
                 globalThis.ws = ws; globalThis.ready0 = ws.readyState;
                 ws.onopen = () => {{ log.push('open:' + ws.protocol); ws.send('ping'); ws.send(new Uint8Array([1,2,3])); }};
                 ws.onmessage = e => log.push('msg:' + (typeof e.data === 'string' ? e.data : 'bin' + e.data.byteLength));
                 ws.onerror = () => log.push('error'); ws.onclose = e => log.push('close:' + e.code + ':' + e.wasClean);"
            ),
        )
        .unwrap();
    assert_eq!(eval(&mut s, "ready0"), "0");
    pump(&mut s, 120);
    let log = eval(&mut s, "log");
    assert!(log.contains("open:"), "{log}");
    assert!(log.contains("msg:ping"), "{log}");
    assert!(log.contains("msg:bin3"), "{log}");
    s.js.run_script("t", "ws.close(1000, 'done');").unwrap();
    pump(&mut s, 60);
    let log = eval(&mut s, "log");
    assert!(log.contains("close:1000:true"), "{log}");
    assert_eq!(eval(&mut s, "ws.readyState"), "3");
}

#[test]
fn websocket_close_codes_are_validated() {
    // Deterministic handshake failure: a plain HTTP server answers the WS upgrade with 200 OK,
    // so the client handshake fails fast regardless of TCP refused/blackhole timing.
    let (port, _server) = http_server(|_| {
        let body = "not a websocket";
        (format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len()).into_bytes(), body.as_bytes().to_vec())
    });
    let mut s = session("<html><body></body></html>", "");
    s.js
        .run_script(
            "t",
            &format!("globalThis.log = []; globalThis.ws = new WebSocket('ws://127.0.0.1:{port}/'); ws.onerror = () => log.push('error'); ws.onclose = e => log.push('close:' + e.code + ':' + e.wasClean);"),
        )
        .unwrap();
    pump(&mut s, 150);
    let log = eval(&mut s, "log");
    assert!(log.contains("error") && log.contains("close:1006:false"), "{log}");
    assert_eq!(eval(&mut s, "ws.readyState"), "3");
    assert!(log.contains("error") && log.contains("close:1006:false"), "{log}");
    assert_eq!(eval(&mut s, "ws.readyState"), "3");
    // Close-code validation happens while CONNECTING (synchronous script: no events interleave).
    s.js.run_script("t", &format!("globalThis.w2 = new WebSocket('ws://127.0.0.1:{port}/');")).unwrap();
    s.js
        .run_script(
            "t",
            "globalThis.codeErrs = [];
             for (const code of [1006, 2000]) { try { w2.close(code); codeErrs.push('none'); } catch (e) { codeErrs.push(e.name); } }
             try { w2.close(1000, 'x'.repeat(124)); codeErrs.push('none'); } catch (e) { codeErrs.push(e.name); }
             w2.close(1000, 'bye');",
        )
        .unwrap();
    assert_eq!(eval(&mut s, "codeErrs"), r#"["InvalidAccessError","InvalidAccessError","SyntaxError"]"#);
    assert_eq!(eval(&mut s, "w2.readyState"), "2");
}

#[test]
fn multi_window_sessions_are_isolated_and_requests_queue() {
    let html = "<html><body><script type=module src=main.js></script></body></html>";
    let js = "globalThis.n = (globalThis.n || 0) + 1;";
    let b = bundle(html, js);
    let mut a = session(html, js);
    let mut c = Session::new(&b, Box::new(MemoryClipboard::default()), (400.0, 300.0, 1.0)).unwrap();
    c.js.host.borrow_mut().echo_console = false;
    c.frame();
    // Each window boots its own JS context: exactly one top-level evaluation each.
    assert_eq!(eval(&mut a, "n"), "1");
    assert_eq!(eval(&mut c, "n"), "1");
    // `window.open` never blocks: it queues a request and returns a live handle.
    a.js.run_script("t", "globalThis.child = window.open('', '_blank', 'width=800,height=600');").unwrap();
    a.frame();
    assert_eq!(eval(&mut a, "[typeof child, child.closed]"), r#"["object",false]"#);
    assert_eq!(a.js.host.borrow().window_open_requests.len(), 1);
    let req = a.js.host.borrow().window_open_requests[0].clone();
    assert_eq!((req.target.as_str(), req.features.as_str()), ("_blank", "width=800,height=600"));
    a.js.run_script("t", "child.close();").unwrap();
    a.frame();
    assert_eq!(eval(&mut a, "child.closed"), "true");
    assert_eq!(a.js.host.borrow().window_close_requests, vec![1]);
}

#[test]
fn a11y_snapshot_lists_roles_names_and_focus() {
    let mut s = session(
        "<html><body><h1>Title</h1><label for=name>Your name</label><input id=name type=text><button id=go>Go</button></body></html>",
        "document.getElementById('go').focus();",
    );
    pump(&mut s, 5);
    let json: serde_json::Value = {
        use rendrix_js::Host;
        let out = s.js.host.borrow_mut().call("a11y_snapshot", &[]).unwrap();
        match out {
            rendrix_js::Val::Str(text) => serde_json::from_str(&text).unwrap(),
            other => panic!("snapshot -> {other:?}"),
        }
    };
    let items = json.as_array().unwrap();
    let has = |role: &str, name: &str| items.iter().any(|n| n["role"] == role && (name.is_empty() || n["name"] == name));
    assert!(has("Window", ""), "{json}");
    assert!(has("Heading", "Title"), "{json}");
    assert!(has("TextInput", "Your name"), "{json}");
    assert!(has("Button", "Go"), "{json}");
}

#[test]
fn update_check_reports_availability_from_a_manifest() {
    let (port, _server) = http_server(|req| {
        let path = req.lines().next().unwrap_or("");
        let manifest = if path.contains("/new") { r#"{"version":"9.9","url":"https://x/app","sha256":"abc"}"# } else { r#"{"version":"0.0.0","url":"https://x/app","sha256":"abc"}"# };
        (format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n", manifest.len()).into_bytes(), manifest.as_bytes().to_vec())
    });
    let mut s = session("<html><body></body></html>", "");
    s.js.host.borrow_mut().permissions.network = true;
    s.js
        .run_script(
            "t",
            &format!(
                "rendrix.update.check('http://127.0.0.1:{port}/new', '1.0.0').then(m => {{ globalThis.found = m; }});
                 rendrix.update.check('http://127.0.0.1:{port}/same', '1.0.0').then(m => {{ globalThis.same = m; }});"
            ),
        )
        .unwrap();
    pump(&mut s, 120);
    assert_eq!(eval(&mut s, "[found.updateAvailable, found.version]"), r#"[true,"9.9"]"#);
    assert_eq!(eval(&mut s, "[same.updateAvailable, same.version]"), r#"[false,"0.0.0"]"#);
}

#[test]
fn update_download_verifies_the_hash_before_staging() {
    let payload = b"fake-artifact-bytes".to_vec();
    let (port, _server) = http_server(move |_| {
        (format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", payload.len()).into_bytes(), payload.clone())
    });
    let mut s = session("<html><body></body></html>", "");
    s.js.host.borrow_mut().permissions.network = true;
    // Wrong hash: the promise rejects with a mismatch error, nothing staged.
    s.js
        .run_script("t", &format!("rendrix.update.download('http://127.0.0.1:{port}/x', 'deadbeef').then(m => {{ globalThis.bad = m; }}, e => {{ globalThis.bad = String(e); }});"))
        .unwrap();
    pump(&mut s, 120);
    let bad = eval(&mut s, "bad");
    assert!(bad.contains("SHA-256 mismatch"), "{bad}");
    // Empty hash (development flow): staged to disk with bytes intact.
    s.js
        .run_script("t", &format!("rendrix.update.download('http://127.0.0.1:{port}/x', '').then(m => {{ globalThis.good = m; }});"))
        .unwrap();
    pump(&mut s, 120);
    let staged = eval(&mut s, "good.downloaded");
    let bytes = std::fs::read(&staged).unwrap();
    assert_eq!(bytes, b"fake-artifact-bytes");
    std::fs::remove_file(&staged).ok();
}

#[test]
fn session_cookie_jar_round_trip() {
    let (port, _server) = http_server(|req| {
        let cookie = req.lines().find(|l| l.to_ascii_lowercase().starts_with("cookie:")).unwrap_or("no-cookie").to_string();
        let body = if cookie == "no-cookie" { "no-cookie:first".to_string() } else { format!("{cookie}:second") };
        let mut head = format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n", body.len());
        if cookie == "no-cookie" {
            head.push_str("Set-Cookie: session=abc123; Path=/\r\n");
        }
        (format!("{head}\r\n").into_bytes(), body.into_bytes())
    });
    let mut s = session("<html><body></body></html>", "");
    s.js.host.borrow_mut().permissions.network = true;
    s.js
        .run_script(
            "t",
            &format!(
                "fetch('http://127.0.0.1:{port}/a').then(r => r.text()).then(t => {{ globalThis.first = t; return fetch('http://127.0.0.1:{port}/b').then(r => r.text()); }}).then(t => {{ globalThis.second = t; }});"
            ),
        )
        .unwrap();
    pump(&mut s, 120);
    assert_eq!(eval(&mut s, "first"), "no-cookie:first");
    assert!(eval(&mut s, "second").contains("session=abc123"), "{}", eval(&mut s, "second"));
}
