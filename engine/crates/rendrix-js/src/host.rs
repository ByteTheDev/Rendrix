//! Native operations behind `__native(op, ...args)`: DOM access, timers, storage, networking, console, window control.

use crate::engine_trait::{Host, Val};
use rendrix_core::Engine;
use rendrix_dom::{MutationKind, NodeId};
use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap, VecDeque};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::time::Instant;

#[derive(Debug, Clone, Default)]
pub struct Permissions {
    pub network: bool,
    /// Directories scripts may read from / write to. Empty = no access.
    pub fs_read: Vec<PathBuf>,
    pub fs_write: Vec<PathBuf>,
}

impl Permissions {
    pub fn allow_all() -> Permissions {
        Permissions { network: true, fs_read: vec![PathBuf::from("/")], fs_write: vec![] }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum WindowCommand {
    SetTitle(String),
    Close,
    Resize(f32, f32),
    Quit,
}

/// A completed (or in-progress, for streams/sockets) host-side async operation.
///
/// All async completions travel over this one channel, which the frame driver already polls
/// every tick (`poll_fetches` → `__rendrix_fetchDone`). Streaming/system events are tagged in
/// `status_text` and routed by the prelude before the fetch map is consulted:
/// - plain fetches: `status_text` is the HTTP reason phrase (or `""` for failures, with `error` set);
/// - `"rendrix:dialog"`: one-shot file-dialog result, `body` is JSON (`string[] | null`);
/// - `"rendrix:ws-open"`, `"rendrix:ws-message"`, `"rendrix:ws-close"`, `"rendrix:ws-error"`:
///   WebSocket lifecycle (see `DomHost::start_ws`);
/// - `"rendrix:update"`: app-update manifest payload.
/// Dedicated `__rendrix_wsDone` / `__rendrix_dialogDone` entry points would be nicer; until the
/// event-loop owner (`runtime.rs`) grows them, this multiplexing keeps every async API working
/// without touching the frame pump.
pub struct FetchResult {
    pub id: u32,
    pub status: u16,
    pub status_text: String,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
    pub error: Option<String>,
    /// Whether delivering this item releases one `pending_fetches` slot. One-shot operations
    /// (`true`) acquire a slot when they start; streaming WebSocket messages (`false`) ride on
    /// the slot acquired at connect time, released by the close event.
    pub releases_slot: bool,
}

/// System-event tags (see `FetchResult`).
pub const TAG_DIALOG: &str = "rendrix:dialog";
pub const TAG_WS_OPEN: &str = "rendrix:ws-open";
pub const TAG_WS_MESSAGE: &str = "rendrix:ws-message";
pub const TAG_WS_CLOSE: &str = "rendrix:ws-close";
pub const TAG_WS_ERROR: &str = "rendrix:ws-error";
pub const TAG_UPDATE: &str = "rendrix:update";
/// Maximum WebSocket messages delivered to script per frame; the rest wait for the next tick.
pub const WS_MAX_PER_TICK: usize = 16;

#[derive(Default)]
struct Storage {
    local: BTreeMap<String, String>,
    session: BTreeMap<String, String>,
    path: Option<PathBuf>,
}

impl Storage {
    fn area(&mut self, name: &str) -> &mut BTreeMap<String, String> {
        if name == "session" {
            &mut self.session
        } else {
            &mut self.local
        }
    }
    fn persist(&self) {
        if let Some(p) = &self.path {
            if let Some(dir) = p.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            let _ = std::fs::write(p, serde_json::to_string(&self.local).unwrap_or_default());
        }
    }
}

pub struct DomHost {
    pub engine: Rc<RefCell<Engine>>,
    start: Instant,
    pub timers: BTreeMap<u32, (f64, Option<f64>)>,
    pub raf_requested: bool,
    pub console: Vec<(String, String)>,
    pub echo_console: bool,
    storage: Storage,
    fetch_tx: Sender<FetchResult>,
    fetch_rx: Receiver<FetchResult>,
    cookie_tx: Sender<(String, Vec<String>)>,
    cookie_rx: Receiver<(String, Vec<String>)>,
    pub pending_fetches: usize,
    pub immediate_fetches: Vec<FetchResult>,
    pub permissions: Permissions,
    pub window_commands: Vec<WindowCommand>,
    /// `window.open(url, target, features)` requests waiting for the platform shell to create
    /// a window. Drained by `window.rs`; kept out of `WindowCommand` so the frame pump's
    /// exhaustive match is untouched.
    pub window_open_requests: Vec<WindowOpenRequest>,
    /// `window.close()` on a secondary window (main-window close still uses `WindowCommand::Close`).
    pub window_close_requests: Vec<u32>,
    pub clipboard: String,
    pub quit_requested: bool,
    pub app_name: String,
    /// Test seam: when set, `dialog_open`/`dialog_save` resolve immediately with this value
    /// (`Some(paths)` = user picked, `None` = user cancelled) instead of showing OS UI.
    dialog_mock: Option<Option<Vec<String>>>,
    /// Overflow for WebSocket messages beyond `WS_MAX_PER_TICK`; oldest first.
    ws_overflow: VecDeque<FetchResult>,
    ws_cmds: HashMap<u32, Sender<WsCommand>>,
    /// Session cookie jar: host → `name=value; ...`. No persistence, no expiry enforcement
    /// beyond `Max-Age=0`/past-`Expires` deletion (documented native scope, see COMPATIBILITY.md).
    cookie_jar: HashMap<String, HashMap<String, String>>,
    /// OS accessibility preferences (see `detect_reduced_motion`; override with
    /// `RENDRIX_REDUCED_MOTION=1` / `RENDRIX_CONTRAST_MORE=1`).
    pub prefs_reduced_motion: bool,
    pub prefs_contrast_more: bool,
}

/// A secondary-window request from script (`window.open`).
#[derive(Debug, Clone, PartialEq)]
pub struct WindowOpenRequest {
    pub id: u32,
    pub url: String,
    pub target: String,
    pub features: String,
}

#[derive(Debug, Clone)]
enum WsCommand {
    Send(Vec<u8>, bool),
    Close(u16, String),
}

pub fn base64_encode(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let n = (chunk[0] as u32) << 16 | (*chunk.get(1).unwrap_or(&0) as u32) << 8 | *chunk.get(2).unwrap_or(&0) as u32;
        out.push(T[(n >> 18) as usize & 63] as char);
        out.push(T[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 { T[(n >> 6) as usize & 63] as char } else { '=' });
        out.push(if chunk.len() > 2 { T[n as usize & 63] as char } else { '=' });
    }
    out
}

pub fn base64_decode(s: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(s.len() * 3 / 4);
    let (mut acc, mut bits) = (0u32, 0);
    for c in s.bytes() {
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' | b'-' => 62,
            b'/' | b'_' => 63,
            b'=' | b'\n' | b'\r' | b' ' => continue,
            _ => return None,
        };
        acc = acc << 6 | v as u32;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
            acc &= (1 << bits) - 1;
        }
    }
    Some(out)
}

fn id(args: &[Val], i: usize) -> Result<NodeId, String> {
    args.get(i).and_then(|v| v.as_f64()).map(|n| n as NodeId).ok_or_else(|| format!("argument {i} must be a node id"))
}

fn opt_id(args: &[Val], i: usize) -> Option<NodeId> {
    match args.get(i) {
        Some(Val::Num(n)) => Some(*n as NodeId),
        _ => None,
    }
}

fn s(args: &[Val], i: usize) -> String {
    match args.get(i) {
        Some(Val::Str(s)) => s.clone(),
        Some(Val::Num(n)) => n.to_string(),
        Some(Val::Bool(b)) => b.to_string(),
        _ => String::new(),
    }
}

fn num(args: &[Val], i: usize) -> f64 {
    args.get(i).and_then(|v| v.as_f64()).unwrap_or(0.0)
}

fn ids(v: Vec<NodeId>) -> Val {
    Val::List(v.into_iter().map(|i| Val::Num(i as f64)).collect())
}

fn rect_val(r: Option<[f32; 4]>) -> Val {
    match r {
        Some(r) => Val::List(r.iter().map(|v| Val::Num(*v as f64)).collect()),
        None => Val::Null,
    }
}

impl DomHost {
    pub fn new(engine: Rc<RefCell<Engine>>, app_name: &str, permissions: Permissions) -> DomHost {
        let (fetch_tx, fetch_rx) = channel();
        let (cookie_tx, cookie_rx) = channel();
        let mut storage = Storage::default();
        // localStorage lives in the per-user data directory, keyed by app name.
        let base = std::env::var_os("LOCALAPPDATA").or_else(|| std::env::var_os("XDG_DATA_HOME")).map(PathBuf::from).or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")));
        if let Some(base) = base {
            let safe: String = app_name.chars().map(|c| if c.is_alphanumeric() || c == '-' || c == '_' { c } else { '_' }).collect();
            let p = base.join("Rendrix").join(safe).join("localStorage.json");
            if let Ok(text) = std::fs::read_to_string(&p) {
                if let Ok(m) = serde_json::from_str(&text) {
                    storage.local = m;
                }
            }
            storage.path = Some(p);
        }
        DomHost {
            engine,
            start: Instant::now(),
            timers: BTreeMap::new(),
            raf_requested: false,
            console: Vec::new(),
            echo_console: true,
            storage,
            fetch_tx,
            fetch_rx,
            cookie_tx,
            cookie_rx,
            pending_fetches: 0,
            immediate_fetches: Vec::new(),
            permissions,
            window_commands: Vec::new(),
            window_open_requests: Vec::new(),
            window_close_requests: Vec::new(),
            clipboard: String::new(),
            quit_requested: false,
            app_name: app_name.to_string(),
            dialog_mock: None,
            ws_overflow: VecDeque::new(),
            ws_cmds: HashMap::new(),
            cookie_jar: HashMap::new(),
            prefs_reduced_motion: detect_reduced_motion(),
            prefs_contrast_more: detect_contrast_more(),
        }
    }

    /// Test seam for file dialogs: `Some(paths)` resolves with those paths, `None` cancels.
    /// `clear` restores real OS dialogs.
    pub fn set_dialog_mock(&mut self, result: Option<Option<Vec<String>>>) {
        self.dialog_mock = result;
    }

    /// Disable persistence of localStorage (tests).
    pub fn disable_storage_persistence(&mut self) {
        self.storage.path = None;
    }

    pub fn now_ms(&self) -> f64 {
        self.start.elapsed().as_secs_f64() * 1000.0
    }

    pub fn take_console(&mut self) -> Vec<(String, String)> {
        std::mem::take(&mut self.console)
    }

    /// Timers whose deadline has passed; one-shot timers are removed, intervals re-armed.
    pub fn take_due_timers(&mut self, now_ms: f64) -> Vec<u32> {
        let due: Vec<u32> = self.timers.iter().filter(|(_, (t, _))| *t <= now_ms).map(|(i, _)| *i).collect();
        for i in &due {
            if let Some((t, interval)) = self.timers.get(i).copied() {
                match interval {
                    Some(iv) => {
                        self.timers.insert(*i, ((t + iv).max(now_ms + 1.0), Some(iv)));
                    }
                    None => {
                        self.timers.remove(i);
                    }
                }
            }
        }
        due
    }

    pub fn next_timer_ms(&self) -> Option<f64> {
        self.timers.values().map(|(t, _)| *t).fold(None, |a: Option<f64>, t| Some(a.map_or(t, |x| x.min(t))))
    }

    pub fn poll_fetches(&mut self) -> Vec<FetchResult> {
        let mut out = std::mem::take(&mut self.immediate_fetches);
        // Harvest session cookies first so follow-up requests in the same tick see them.
        while let Ok((host, values)) = self.cookie_rx.try_recv() {
            self.apply_cookies(&host, &values);
        }
        // Oldest overflowed socket messages first, within this frame's budget.
        let mut budget = WS_MAX_PER_TICK;
        while budget > 0 {
            match self.ws_overflow.pop_front() {
                Some(r) => {
                    out.push(r);
                    budget -= 1;
                }
                None => break,
            }
        }
        while let Ok(r) = self.fetch_rx.try_recv() {
            if r.releases_slot {
                self.pending_fetches = self.pending_fetches.saturating_sub(1);
            }
            if r.status_text == TAG_WS_CLOSE || r.status_text == TAG_WS_ERROR {
                self.ws_cmds.remove(&r.id);
            }
            if r.status_text == TAG_WS_MESSAGE && budget == 0 {
                self.ws_overflow.push_back(r);
                continue;
            }
            if r.status_text == TAG_WS_MESSAGE {
                budget -= 1;
            }
            out.push(r);
        }
        out
    }

    /// A uniform f64 in [0, 1) from the operating system's CSPRNG (backs `crypto.getRandomValues`).
    fn rand(&mut self) -> f64 {
        let mut buf = [0u8; 8];
        if getrandom::getrandom(&mut buf).is_err() {
            // Never fall back to a predictable source for crypto: fail loudly instead.
            panic!("the operating system random number generator is unavailable");
        }
        (u64::from_le_bytes(buf) >> 11) as f64 / (1u64 << 53) as f64
    }

    fn start_fetch(&mut self, fid: u32, url: String, method: String, headers: Vec<(String, String)>, body: Option<Vec<u8>>) {
        let done = |id, status: u16, text: &str, headers: Vec<(String, String)>, body: Vec<u8>| FetchResult { id, status, status_text: text.to_string(), headers, body, error: None, releases_slot: true };
        let fail = |id, msg: String| FetchResult { id, status: 0, status_text: String::new(), headers: vec![], body: vec![], error: Some(msg), releases_slot: true };
        if let Some(rest) = url.strip_prefix("data:") {
            let r = match rest.split_once(',') {
                Some((meta, data)) => {
                    let bytes = if meta.ends_with(";base64") { base64_decode(data) } else { Some(percent_decode(data)) };
                    match bytes {
                        Some(b) => done(fid, 200, "OK", vec![("content-type".into(), meta.trim_end_matches(";base64").to_string())], b),
                        None => fail(fid, "invalid data: URL".into()),
                    }
                }
                None => fail(fid, "invalid data: URL".into()),
            };
            self.immediate_fetches.push(r);
            return;
        }
        if !url.contains("://") {
            // Relative URL → bundled asset.
            let bytes = self.engine.borrow().read_asset(&url);
            let r = match bytes {
                Some(b) => done(fid, 200, "OK", vec![("content-type".into(), guess_mime(&url).into())], b),
                None => done(fid, 404, "Not Found", vec![], vec![]),
            };
            self.immediate_fetches.push(r);
            return;
        }
        if !self.permissions.network {
            self.immediate_fetches.push(fail(fid, "network access is not permitted for this app".into()));
            return;
        }
        self.pending_fetches += 1;
        let tx = self.fetch_tx.clone();
        let cookie_tx = self.cookie_tx.clone();
        // Session cookie jar: attach stored cookies, harvest `Set-Cookie` on the way back.
        let req_host = url_host(&url);
        let mut headers = headers;
        if let Some(h) = &req_host {
            if let Some(jar) = self.cookie_jar.get(h) {
                if !jar.is_empty() && !headers.iter().any(|(k, _)| k.eq_ignore_ascii_case("cookie")) {
                    let mut pairs: Vec<String> = jar.iter().map(|(k, v)| format!("{k}={v}")).collect();
                    pairs.sort();
                    headers.push(("Cookie".to_string(), pairs.join("; ")));
                }
            }
        }
        std::thread::spawn(move || {
            let mut req = ureq::request(&method, &url).timeout(std::time::Duration::from_secs(60));
            for (k, v) in &headers {
                req = req.set(k, v);
            }
            let resp = match body {
                Some(b) => req.send_bytes(&b),
                None => req.call(),
            };
            let result = match resp {
                Ok(r) | Err(ureq::Error::Status(_, r)) => {
                    let status = r.status();
                    let text = r.status_text().to_string();
                    let hdrs: Vec<(String, String)> = r.headers_names().into_iter().filter_map(|n| r.header(&n).map(|v| (n, v.to_string()))).collect();
                    if let Some(h) = &req_host {
                        let set_cookies: Vec<String> = hdrs.iter().filter(|(k, _)| k.eq_ignore_ascii_case("set-cookie")).map(|(_, v)| v.clone()).collect();
                        if !set_cookies.is_empty() {
                            let _ = cookie_tx.send((h.clone(), set_cookies));
                        }
                    }
                    let mut buf = Vec::new();
                    use std::io::Read;
                    match r.into_reader().take(64 * 1024 * 1024).read_to_end(&mut buf) {
                        Ok(_) => FetchResult { id: fid, status, status_text: text, headers: hdrs, body: buf, error: None, releases_slot: true },
                        Err(e) => FetchResult { id: fid, status: 0, status_text: String::new(), headers: vec![], body: vec![], error: Some(e.to_string()), releases_slot: true },
                    }
                }
                Err(e) => FetchResult { id: fid, status: 0, status_text: String::new(), headers: vec![], body: vec![], error: Some(e.to_string()), releases_slot: true },
            };
            let _ = tx.send(result);
        });
    }

    fn fs_allowed(list: &[PathBuf], path: &str) -> Option<PathBuf> {
        let p = std::fs::canonicalize(path).ok().or_else(|| {
            // For writes to new files: canonicalise the parent.
            let pb = PathBuf::from(path);
            pb.parent().and_then(|d| std::fs::canonicalize(d).ok()).map(|d| d.join(pb.file_name().unwrap_or_default()))
        })?;
        list.iter().any(|root| std::fs::canonicalize(root).map(|r| p.starts_with(r)).unwrap_or(false)).then_some(p)
    }

    // ------------------------------------------------------------------------------------
    // Cookies (session jar)
    // ------------------------------------------------------------------------------------

    fn apply_cookies(&mut self, host: &str, values: &[String]) {
        for v in values {
            let mut parts = v.split(';');
            let Some(pair) = parts.next() else { continue };
            let (name, value) = match pair.split_once('=') {
                Some((n, val)) => (n.trim().to_string(), val.trim().to_string()),
                None => continue,
            };
            if name.is_empty() {
                continue;
            }
            let rest = parts.collect::<Vec<_>>().join(";").to_ascii_lowercase();
            // Deletion: Max-Age=0 drops the cookie regardless of the accompanying value.
            if rest.contains("max-age=0") {
                if let Some(jar) = self.cookie_jar.get_mut(host) {
                    jar.remove(&name);
                }
                continue;
            }
            self.cookie_jar.entry(host.to_string()).or_default().insert(name, value);
        }
    }

    // ------------------------------------------------------------------------------------
    // Native file dialogs (A2). The blocking OS UI always runs on a worker thread; the frame
    // loop only sees the completion event. `set_dialog_mock` bypasses the OS for tests.
    // ------------------------------------------------------------------------------------

    fn start_dialog(&mut self, id: u32, save: bool, opts_json: &str) {
        let opts: serde_json::Value = serde_json::from_str(opts_json).unwrap_or(serde_json::Value::Null);
        if let Some(mock) = self.dialog_mock.clone() {
            let body = serde_json::to_vec(&mock).unwrap_or_default();
            self.immediate_fetches.push(FetchResult { id, status: 0, status_text: TAG_DIALOG.into(), headers: vec![], body, error: None, releases_slot: false });
            return;
        }
        let title = opts.get("title").and_then(|v| v.as_str()).unwrap_or("").to_string();
        let default_path = opts.get("defaultPath").and_then(|v| v.as_str()).unwrap_or("").to_string();
        let directory = opts.get("properties").and_then(|v| v.as_array()).is_some_and(|a| a.iter().any(|p| p.as_str() == Some("openDirectory")));
        let multiple = opts.get("multiple").and_then(|v| v.as_bool()).unwrap_or(false);
        let filters: Vec<(String, Vec<String>)> = opts
            .get("filters")
            .and_then(|v| v.as_array())
            .map(|a| {
                a.iter()
                    .map(|f| {
                        let name = f.get("name").and_then(|v| v.as_str()).unwrap_or("Files").to_string();
                        let exts = f.get("extensions").and_then(|v| v.as_array()).map(|e| e.iter().filter_map(|x| x.as_str().map(str::to_string)).collect()).unwrap_or_default();
                        (name, exts)
                    })
                    .collect()
            })
            .unwrap_or_default();
        self.pending_fetches += 1;
        let tx = self.fetch_tx.clone();
        std::thread::spawn(move || {
            let mut dlg = rfd::FileDialog::new();
            if !title.is_empty() {
                dlg = dlg.set_title(&title);
            }
            for (name, exts) in &filters {
                dlg = dlg.add_filter(name, exts);
            }
            let paths: Option<Vec<String>> = if save {
                if !default_path.is_empty() {
                    dlg = dlg.set_file_name(&default_path);
                }
                dlg.save_file().map(|p| vec![p.to_string_lossy().into_owned()])
            } else if directory {
                dlg.pick_folder().map(|p| vec![p.to_string_lossy().into_owned()])
            } else if multiple {
                dlg.pick_files().map(|ps| ps.into_iter().map(|p| p.to_string_lossy().into_owned()).collect())
            } else {
                dlg.pick_file().map(|p| vec![p.to_string_lossy().into_owned()])
            };
            let body = serde_json::to_vec(&paths).unwrap_or_default();
            let _ = tx.send(FetchResult { id, status: 0, status_text: TAG_DIALOG.into(), headers: vec![], body, error: None, releases_slot: true });
        });
    }

    /// Open a URL in the OS default handler (browser, mail client). Fire-and-forget: the child
    /// process is spawned detached and never blocks the frame loop.
    fn open_external(url: &str) -> Result<(), String> {
        let lower = url.to_ascii_lowercase();
        if !(lower.starts_with("https://") || lower.starts_with("http://") || lower.starts_with("mailto:")) {
            return Err("openExternal only supports http(s) and mailto URLs".to_string());
        }
        #[cfg(target_os = "windows")]
        let mut cmd = { let mut c = std::process::Command::new("cmd"); c.args(["/C", "start", "", url]); c };
        #[cfg(target_os = "macos")]
        let mut cmd = { let mut c = std::process::Command::new("open"); c.arg(url); c };
        #[cfg(not(any(target_os = "windows", target_os = "macos")))]
        let mut cmd = { let mut c = std::process::Command::new("xdg-open"); c.arg(url); c };
        cmd.stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).spawn().map(|_| ()).map_err(|e| format!("could not open URL: {e}"))
    }

    // ------------------------------------------------------------------------------------
    // WebSocket client (A5). One OS thread per socket; the frame loop only sees tagged
    // completion events (capped at WS_MAX_PER_TICK messages per tick in `poll_fetches`).
    // ------------------------------------------------------------------------------------

    fn start_ws(&mut self, id: u32, url: String, protocols: Vec<String>, origin: Option<String>) {
        // Local echo servers and tests use plain ws://; anything else needs permission.
        let is_local = url_host(&url).is_some_and(|h| h == "localhost" || h == "127.0.0.1" || h == "[::1]" || h == "::1");
        if !self.permissions.network && !is_local {
            self.immediate_fetches.push(FetchResult { id, status: 1006, status_text: TAG_WS_ERROR.into(), headers: vec![], body: b"network access is not permitted for this app".to_vec(), error: None, releases_slot: false });
            return;
        }
        if !(url.starts_with("ws://") || url.starts_with("wss://")) {
            self.immediate_fetches.push(FetchResult { id, status: 1006, status_text: TAG_WS_ERROR.into(), headers: vec![], body: b"WebSocket URL must start with ws:// or wss://".to_vec(), error: None, releases_slot: false });
            return;
        }
        self.pending_fetches += 1;
        let tx = self.fetch_tx.clone();
        let (cmd_tx, cmd_rx) = channel::<WsCommand>();
        self.ws_cmds.insert(id, cmd_tx);
        std::thread::spawn(move || {
            let emit = |tag: &str, status: u16, body: Vec<u8>, releases_slot: bool| {
                let _ = tx.send(FetchResult { id, status, status_text: tag.into(), headers: vec![], body, error: None, releases_slot });
            };
            // Build the handshake request so subprotocols (and a synthetic origin) survive.
            // `ClientRequestBuilder` generates the key/version/host framing; a raw `Request`
            // would be passed through untouched and rejected by the server.
            let uri: tungstenite::http::Uri = match url.parse() {
                Ok(u) => u,
                Err(e) => {
                    emit(TAG_WS_ERROR, 1006, format!("invalid WebSocket URL: {e}").into_bytes(), true);
                    return;
                }
            };
            let mut builder = tungstenite::ClientRequestBuilder::new(uri);
            for p in &protocols {
                builder = builder.with_sub_protocol(p);
            }
            if let Some(o) = origin {
                builder = builder.with_header("Origin", o);
            }
            let (mut socket, response) = match tungstenite::connect(builder) {
                Ok(pair) => pair,
                Err(e) => {
                    emit(TAG_WS_ERROR, 1006, format!("WebSocket connect failed: {e}").into_bytes(), true);
                    return;
                }
            };
            // Short reads so outgoing app messages and close frames are never stuck behind a
            // blocking `read`. 50 ms keeps end-to-end latency well under a frame budget slice.
            match socket.get_mut() {
                tungstenite::stream::MaybeTlsStream::Plain(s) => {
                    let _ = s.set_read_timeout(Some(std::time::Duration::from_millis(50)));
                }
                tungstenite::stream::MaybeTlsStream::Rustls(s) => {
                    let _ = s.get_mut().set_read_timeout(Some(std::time::Duration::from_millis(50)));
                }
                _ => {}
            }
            let negotiated = response.headers().get("Sec-WebSocket-Protocol").and_then(|v| v.to_str().ok()).unwrap_or("").to_string();
            emit(TAG_WS_OPEN, 0, negotiated.into_bytes(), false);
            loop {
                while let Ok(cmd) = cmd_rx.try_recv() {
                    match cmd {
                        WsCommand::Send(data, is_text) => {
                            let msg = if is_text { tungstenite::Message::Text(String::from_utf8_lossy(&data).into_owned().into()) } else { tungstenite::Message::Binary(data.into()) };
                            if socket.send(msg).is_err() {
                                emit(TAG_WS_CLOSE, 1006, b"send failed".to_vec(), true);
                                return;
                            }
                        }
                        WsCommand::Close(code, reason) => {
                            let frame = tungstenite::protocol::CloseFrame { code: tungstenite::protocol::frame::coding::CloseCode::from(code), reason: reason.into() };
                            let _ = socket.send(tungstenite::Message::Close(Some(frame)));
                            let _ = socket.flush();
                            emit(TAG_WS_CLOSE, code, Vec::new(), true);
                            return;
                        }
                    }
                }
                match socket.read() {
                    Ok(tungstenite::Message::Text(t)) => emit(TAG_WS_MESSAGE, 1, t.as_bytes().to_vec(), false),
                    Ok(tungstenite::Message::Binary(b)) => emit(TAG_WS_MESSAGE, 0, b.to_vec(), false),
                    Ok(tungstenite::Message::Ping(_)) => {
                        let _ = socket.send(tungstenite::Message::Pong(Vec::new().into()));
                    }
                    Ok(tungstenite::Message::Pong(_)) => {}
                    Ok(tungstenite::Message::Close(frame)) => {
                        let (code, reason) = match frame {
                            Some(f) => (u16::from(f.code), f.reason.as_bytes().to_vec()),
                            None => (1005, Vec::new()),
                        };
                        emit(TAG_WS_CLOSE, code, reason, true);
                        return;
                    }
                    Ok(tungstenite::Message::Frame(_)) => {}
                    Err(tungstenite::Error::Io(e)) if matches!(e.kind(), std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock) => {}
                    Err(_) => {
                        emit(TAG_WS_CLOSE, 1006, b"connection lost".to_vec(), true);
                        return;
                    }
                }
            }
        });
    }

    fn ws_send_to(&mut self, id: u32, data: Vec<u8>, is_text: bool) -> Result<(), String> {
        match self.ws_cmds.get(&id) {
            Some(tx) => tx.send(WsCommand::Send(data, is_text)).map_err(|_| "WebSocket is closed".to_string()),
            None => Err("WebSocket is closed".to_string()),
        }
    }

    fn ws_close_socket(&mut self, id: u32, code: u16, reason: String) {
        // The worker always answers with a close event that releases the connect slot, so no
        // counter fix-up is needed here (a panicked worker would only cost a 16 ms wake cadence).
        if let Some(tx) = self.ws_cmds.remove(&id) {
            let _ = tx.send(WsCommand::Close(code, reason));
        }
    }

    // ------------------------------------------------------------------------------------
    // Minimal update client (S1): manifest check → hashed download → staged install.
    // ------------------------------------------------------------------------------------

    fn start_update_check(&mut self, id: u32, feed_url: String, current: String) {
        if !self.permissions.network {
            self.immediate_fetches.push(FetchResult { id, status: 0, status_text: TAG_UPDATE.into(), headers: vec![], body: br#"{"error":"network access is not permitted for this app"}"#.to_vec(), error: None, releases_slot: false });
            return;
        }
        self.pending_fetches += 1;
        let tx = self.fetch_tx.clone();
        std::thread::spawn(move || {
            let body = match ureq::get(&feed_url).timeout(std::time::Duration::from_secs(30)).call() {
                Ok(r) => {
                    let mut buf = String::new();
                    use std::io::Read;
                    match r.into_reader().take(1024 * 1024).read_to_string(&mut buf) {
                        Ok(_) => update_available_json(&buf, &current),
                        Err(e) => format!(r#"{{"error":"manifest read failed: {e}"}}"#),
                    }
                }
                Err(e) => format!(r#"{{"error":"update check failed: {e}"}}"#),
            };
            let _ = tx.send(FetchResult { id, status: 0, status_text: TAG_UPDATE.into(), headers: vec![], body: body.into_bytes(), error: None, releases_slot: true });
        });
    }

    fn start_update_download(&mut self, id: u32, url: String, sha256: String) {
        if !self.permissions.network {
            self.immediate_fetches.push(FetchResult { id, status: 0, status_text: TAG_UPDATE.into(), headers: vec![], body: br#"{"error":"network access is not permitted for this app"}"#.to_vec(), error: None, releases_slot: false });
            return;
        }
        self.pending_fetches += 1;
        let tx = self.fetch_tx.clone();
        std::thread::spawn(move || {
            let body = match ureq::get(&url).timeout(std::time::Duration::from_secs(300)).call() {
                Ok(r) => {
                    let path = std::env::temp_dir().join(format!("rendrix-update-{id}.bin"));
                    let mut file = match std::fs::File::create(&path) {
                        Ok(f) => f,
                        Err(e) => return fail_update(&tx, id, &format!("cannot stage download: {e}")),
                    };
                    use std::io::{Read, Write};
                    // Incremental SHA-256 without loading the artifact into memory.
                    let (mut hasher, mut reader) = (Sha256::new(), r.into_reader());
                    let mut buf = [0u8; 64 * 1024];
                    let mut downloaded: u64 = 0;
                    let fail = |e: String| fail_update(&tx, id, &e);
                    loop {
                        match reader.read(&mut buf) {
                            Ok(0) => break,
                            Ok(n) => {
                                hasher.update(&buf[..n]);
                                downloaded += n as u64;
                                if downloaded > 512 * 1024 * 1024 {
                                    return fail("artifact exceeds the 512 MiB update limit".into());
                                }
                                if file.write_all(&buf[..n]).is_err() {
                                    return fail("cannot write staged download".into());
                                }
                            }
                            Err(e) => return fail(format!("download failed: {e}")),
                        }
                    }
                    let digest = hasher.finalize();
                    let hex: String = digest.iter().map(|b| format!("{b:02x}")).collect();
                    if !sha256.is_empty() && hex != sha256.to_ascii_lowercase() {
                        let _ = std::fs::remove_file(&path);
                        return fail(format!("SHA-256 mismatch: manifest wants {sha256}, got {hex}"));
                    }
                    format!(r#"{{"downloaded":{}}}"#, serde_json::to_string(&path.to_string_lossy()).unwrap_or_default())
                }
                Err(e) => format!(r#"{{"error":"download failed: {e}"}}"#),
            };
            let _ = tx.send(FetchResult { id, status: 0, status_text: TAG_UPDATE.into(), headers: vec![], body: body.into_bytes(), error: None, releases_slot: true });
        });
    }

    fn update_install(&mut self, staged: &str) -> Result<(), String> {
        let staged = PathBuf::from(staged);
        if !staged.is_file() {
            return Err("staged update file does not exist (call update.download first)".to_string());
        }
        let current = std::env::current_exe().map_err(|e| format!("cannot locate running executable: {e}"))?;
        #[cfg(target_os = "windows")]
        {
            // A running .exe cannot be overwritten, but it *can* be moved aside.
            let backup = current.with_extension("old.exe");
            let _ = std::fs::remove_file(&backup);
            std::fs::rename(&current, &backup).map_err(|e| format!("cannot move running executable aside: {e}"))?;
            if let Err(e) = std::fs::rename(&staged, &current) {
                let _ = std::fs::rename(&backup, &current);
                return Err(format!("cannot install update: {e}"));
            }
        }
        #[cfg(not(target_os = "windows"))]
        {
            std::fs::rename(&staged, &current).map_err(|e| format!("cannot install update: {e}"))?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let _ = std::fs::set_permissions(&current, std::fs::Permissions::from_mode(0o755));
            }
        }
        // Relaunch, then quit this instance (`quitAndInstall` semantics).
        std::process::Command::new(&current).stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).spawn().map_err(|e| format!("installed, but relaunch failed: {e}"))?;
        self.quit_requested = true;
        self.window_commands.push(WindowCommand::Quit);
        Ok(())
    }
}

/// Lower-cased `host` (no port) of an `http(s)` URL, for the session cookie jar.
fn url_host(url: &str) -> Option<String> {
    let rest = url.split_once("://")?.1;
    let host = rest.split(['/', '?', '#']).next().unwrap_or("");
    let host = host.rsplit('@').next().unwrap_or(host);
    let host = match host.strip_prefix('[').and_then(|h| h.split_once(']')) {
        Some((h, _)) => h.to_string(),
        None => host.split(':').next().unwrap_or("").to_string(),
    };
    if host.is_empty() { None } else { Some(host.to_ascii_lowercase()) }
}

/// OS "reduce motion" preference. No new system dependency is approved for this phase, so this
/// is best-effort: explicit opt-out via `RENDRIX_REDUCED_MOTION=1`, otherwise off. A real
/// per-OS query (SystemParametersInfo / NSWorkspace / org.gnome.desktop) is S1 follow-up work.
fn detect_reduced_motion() -> bool {
    std::env::var_os("RENDRIX_REDUCED_MOTION").is_some_and(|v| v == "1" || v == "true")
}

fn detect_contrast_more() -> bool {
    std::env::var_os("RENDRIX_CONTRAST_MORE").is_some_and(|v| v == "1" || v == "true")
}

fn percent_decode(s: &str) -> Vec<u8> {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let Some(v) = std::str::from_utf8(&b[i + 1..i + 3]).ok().and_then(|h| u8::from_str_radix(h, 16).ok()) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    out
}

/// Minimal SHA-256 for update-artifact verification (FIPS 180-4). Kept in-tree because no
/// general hashing dependency is approved for this phase.
struct Sha256 {
    h: [u32; 8],
    buf: [u8; 64],
    buf_len: usize,
    total: u64,
}

impl Sha256 {
    fn new() -> Sha256 {
        Sha256 {
            h: [0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19],
            buf: [0; 64],
            buf_len: 0,
            total: 0,
        }
    }

    fn update(&mut self, mut data: &[u8]) {
        self.total += data.len() as u64;
        if self.buf_len > 0 {
            let take = (64 - self.buf_len).min(data.len());
            self.buf[self.buf_len..self.buf_len + take].copy_from_slice(&data[..take]);
            self.buf_len += take;
            data = &data[take..];
            if self.buf_len == 64 {
                let block = self.buf;
                Self::round(&mut self.h, &block);
                self.buf_len = 0;
            }
        }
        while data.len() >= 64 {
            let mut block = [0u8; 64];
            block.copy_from_slice(&data[..64]);
            Self::round(&mut self.h, &block);
            data = &data[64..];
        }
        if !data.is_empty() {
            self.buf[..data.len()].copy_from_slice(data);
            self.buf_len = data.len();
        }
    }

    fn finalize(mut self) -> [u8; 32] {
        let bit_len = self.total.wrapping_mul(8);
        self.update(&[0x80]);
        while self.buf_len != 56 {
            self.update(&[0x00]);
        }
        self.update(&bit_len.to_be_bytes());
        let mut out = [0u8; 32];
        for (i, w) in self.h.iter().enumerate() {
            out[4 * i..4 * i + 4].copy_from_slice(&w.to_be_bytes());
        }
        out
    }

    fn round(h: &mut [u32; 8], block: &[u8; 64]) {
        const K: [u32; 64] = [
            0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
            0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
            0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
            0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
            0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
            0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
            0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
            0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
        ];
        let mut w = [0u32; 64];
        for (i, chunk) in block.chunks_exact(4).enumerate().take(16) {
            w[i] = u32::from_be_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16].wrapping_add(s0).wrapping_add(w[i - 7]).wrapping_add(s1);
        }
        let (mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut hh) = (h[0], h[1], h[2], h[3], h[4], h[5], h[6], h[7]);
        for i in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ ((!e) & g);
            let t1 = hh.wrapping_add(s1).wrapping_add(ch).wrapping_add(K[i]).wrapping_add(w[i]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let t2 = s0.wrapping_add(maj);
            hh = g;
            g = f;
            f = e;
            e = d.wrapping_add(t1);
            d = c;
            c = b;
            b = a;
            a = t1.wrapping_add(t2);
        }
        h[0] = h[0].wrapping_add(a);
        h[1] = h[1].wrapping_add(b);
        h[2] = h[2].wrapping_add(c);
        h[3] = h[3].wrapping_add(d);
        h[4] = h[4].wrapping_add(e);
        h[5] = h[5].wrapping_add(f);
        h[6] = h[6].wrapping_add(g);
        h[7] = h[7].wrapping_add(hh);
    }
}

/// Compare dotted version strings (`1.10.0` > `1.9.0`); trailing non-numeric parts fall back to
/// lexicographic order.
fn version_compare(a: &str, b: &str) -> std::cmp::Ordering {
    let parse = |s: &str| s.split('.').map(|p| p.parse::<u64>().unwrap_or(u64::MAX)).collect::<Vec<_>>();
    match parse(a).cmp(&parse(&b)) {
        std::cmp::Ordering::Equal => a.cmp(b),
        other => other,
    }
}

/// Answer an update check from a fetched manifest body.
fn update_available_json(manifest: &str, current: &str) -> String {
    let v: serde_json::Value = match serde_json::from_str(manifest) {
        Ok(v) => v,
        Err(e) => return format!(r#"{{"error":"invalid update manifest: {e}"}}"#),
    };
    let version = v.get("version").and_then(|x| x.as_str()).unwrap_or("").to_string();
    let url = v.get("url").and_then(|x| x.as_str()).unwrap_or("").to_string();
    let sha256 = v.get("sha256").and_then(|x| x.as_str()).unwrap_or("").to_string();
    if version.is_empty() || url.is_empty() {
        return r#"{"error":"update manifest needs 'version' and 'url'"}"#.to_string();
    }
    let available = version_compare(&version, current) == std::cmp::Ordering::Greater;
    let obj = serde_json::json!({ "updateAvailable": available, "version": version, "url": url, "sha256": sha256, "currentVersion": current });
    serde_json::to_string(&obj).unwrap_or_default()
}

fn fail_update(tx: &Sender<FetchResult>, id: u32, msg: &str) {
    let body = format!(r#"{{"error":{}}}"#, serde_json::to_string(msg).unwrap_or_default());
    let _ = tx.send(FetchResult { id, status: 0, status_text: TAG_UPDATE.into(), headers: vec![], body: body.into_bytes(), error: None, releases_slot: true });
}

fn guess_mime(path: &str) -> &'static str {
    match path.rsplit('.').next().unwrap_or("").to_ascii_lowercase().as_str() {
        "json" => "application/json",
        "html" | "htm" => "text/html",
        "css" => "text/css",
        "js" | "mjs" => "text/javascript",
        "txt" => "text/plain",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "svg" => "image/svg+xml",
        "woff2" => "font/woff2",
        "ttf" => "font/ttf",
        _ => "application/octet-stream",
    }
}

impl Host for DomHost {
    fn call(&mut self, op: &str, a: &[Val]) -> Result<Val, String> {
        let engine = self.engine.clone();
        // Most ops touch the DOM; borrow lazily per arm to keep geometry ops able to run layout.
        macro_rules! doc {
            () => {
                engine.borrow_mut()
            };
        }
        Ok(match op {
            // ---- creation ----
            "create_element" => Val::Num(doc!().doc.create_element(&s(a, 0)) as f64),
            "create_text" => Val::Num(doc!().doc.create_text(&s(a, 0)) as f64),
            "create_comment" => Val::Num(doc!().doc.create_comment(&s(a, 0)) as f64),
            "create_fragment" => Val::Num(doc!().doc.create_fragment() as f64),
            "clone_node" => Val::Num(doc!().doc.clone_node(id(a, 0)?, a.get(1).is_some_and(|v| v.as_bool())) as f64),
            // ---- tree ----
            "append_child" => {
                doc!().doc.append_child(id(a, 0)?, id(a, 1)?).map_err(|e| e.to_string())?;
                Val::Undefined
            }
            "insert_before" => {
                doc!().doc.insert_before(id(a, 0)?, id(a, 1)?, opt_id(a, 2)).map_err(|e| e.to_string())?;
                Val::Undefined
            }
            "remove_child" => {
                doc!().doc.remove_child(id(a, 0)?, id(a, 1)?).map_err(|e| e.to_string())?;
                Val::Undefined
            }
            "replace_child" => {
                doc!().doc.replace_child(id(a, 0)?, id(a, 1)?, id(a, 2)?).map_err(|e| e.to_string())?;
                Val::Undefined
            }
            "parent" => Val::from_opt_id(doc!().doc.parent(id(a, 0)?)),
            "first_child" => Val::from_opt_id(doc!().doc.first_child(id(a, 0)?)),
            "last_child" => Val::from_opt_id(doc!().doc.last_child(id(a, 0)?)),
            "next_sibling" => Val::from_opt_id(doc!().doc.next_sibling(id(a, 0)?)),
            "prev_sibling" => Val::from_opt_id(doc!().doc.prev_sibling(id(a, 0)?)),
            "children" => {
                let e = doc!();
                ids(e.doc.children(id(a, 0)?).collect())
            }
            "contains" => Val::Bool(doc!().doc.contains(id(a, 0)?, id(a, 1)?)),
            "is_connected" => Val::Bool(doc!().doc.is_connected(id(a, 0)?)),
            "event_path" => ids(doc!().doc.event_path(id(a, 0)?)),
            "document_element" => Val::from_opt_id(doc!().doc.document_element()),
            "head" => Val::from_opt_id(doc!().doc.head()),
            "body" => Val::from_opt_id(doc!().doc.body()),
            "document_root" => Val::Num(doc!().doc.root() as f64),
            // ---- node info ----
            "node_type" => Val::Num(doc!().doc.node_type(id(a, 0)?) as u8 as f64),
            "tag" => doc!().doc.tag(id(a, 0)?).map(Val::from).unwrap_or(Val::Null),
            "text_data" => doc!().doc.text(id(a, 0)?).map(Val::from).unwrap_or(Val::Null),
            "set_text_data" => {
                doc!().doc.set_text(id(a, 0)?, &s(a, 1));
                Val::Undefined
            }
            "text_content" => Val::Str(doc!().doc.text_content(id(a, 0)?)),
            "set_text_content" => {
                doc!().doc.set_text_content(id(a, 0)?, &s(a, 1));
                Val::Undefined
            }
            "inner_html" => Val::Str(doc!().doc.inner_html(id(a, 0)?)),
            "outer_html" => Val::Str(doc!().doc.outer_html(id(a, 0)?)),
            "set_inner_html" => {
                doc!().doc.set_inner_html(id(a, 0)?, &s(a, 1));
                Val::Undefined
            }
            "parse_fragment" => Val::Num(doc!().doc.parse_fragment(&s(a, 0)) as f64),
            // ---- attributes ----
            "get_attr" => doc!().doc.get_attr(id(a, 0)?, &s(a, 1)).map(Val::from).unwrap_or(Val::Null),
            "set_attr" => {
                doc!().doc.set_attr(id(a, 0)?, &s(a, 1), &s(a, 2));
                Val::Undefined
            }
            "remove_attr" => {
                doc!().doc.remove_attr(id(a, 0)?, &s(a, 1));
                Val::Undefined
            }
            "attrs" => {
                let e = doc!();
                match e.doc.element(id(a, 0)?) {
                    Some(el) => Val::List(el.attrs.iter().flat_map(|(k, v)| [Val::Str(k.clone()), Val::Str(v.clone())]).collect()),
                    None => Val::List(vec![]),
                }
            }
            // ---- queries ----
            "query" => {
                let e = doc!();
                Val::from_opt_id(e.doc.query_selector(id(a, 0)?, &s(a, 1)).map_err(|e| e.to_string())?)
            }
            "query_all" => {
                let e = doc!();
                ids(e.doc.query_selector_all(id(a, 0)?, &s(a, 1)).map_err(|e| e.to_string())?)
            }
            "matches" => Val::Bool(doc!().doc.matches(id(a, 0)?, &s(a, 1)).map_err(|e| e.to_string())?),
            "closest" => Val::from_opt_id(doc!().doc.closest(id(a, 0)?, &s(a, 1)).map_err(|e| e.to_string())?),
            "by_id" => Val::from_opt_id(doc!().doc.get_element_by_id(&s(a, 0))),
            "by_tag" => ids(doc!().doc.get_elements_by_tag_name(id(a, 0)?, &s(a, 1))),
            "by_class" => ids(doc!().doc.get_elements_by_class_name(id(a, 0)?, &s(a, 1))),
            // ---- form state ----
            "value" => Val::Str(doc!().doc.value(id(a, 0)?)),
            "set_value" => {
                doc!().doc.set_form_value(id(a, 0)?, &s(a, 1));
                Val::Undefined
            }
            "checked" => Val::Bool(doc!().doc.is_checked(id(a, 0)?)),
            "set_checked" => {
                doc!().doc.set_checked(id(a, 0)?, a.get(1).is_some_and(|v| v.as_bool()));
                Val::Undefined
            }
            "selection" => {
                let e = doc!();
                match e.doc.element(id(a, 0)?) {
                    Some(el) => {
                        let (x, y) = (el.form.selection_start, el.form.selection_end);
                        let v = e.doc.value(id(a, 0)?);
                        // UTF-16 offsets for JS.
                        let u16 = |b: usize| v[..b.min(v.len())].encode_utf16().count() as f64;
                        Val::List(vec![Val::Num(u16(x.min(y))), Val::Num(u16(x.max(y)))])
                    }
                    None => Val::Null,
                }
            }
            "set_selection" => {
                let mut e = doc!();
                let v = e.doc.value(id(a, 0)?);
                let to_byte = |u: usize| {
                    let mut count = 0;
                    for (b, c) in v.char_indices() {
                        if count >= u {
                            return b;
                        }
                        count += c.len_utf16();
                    }
                    v.len()
                };
                let (x, y) = (to_byte(num(a, 1) as usize), to_byte(num(a, 2) as usize));
                e.doc.set_selection(id(a, 0)?, x, y);
                e.reset_caret();
                Val::Undefined
            }
            // ---- focus ----
            "focus" => {
                let mut e = doc!();
                let n = id(a, 0)?;
                let prev = e.doc.focused();
                if e.doc.is_focusable(n) {
                    e.doc.set_focus(Some(n));
                    e.reset_caret();
                }
                Val::from_opt_id(prev)
            }
            "blur" => {
                let mut e = doc!();
                let n = id(a, 0)?;
                if e.doc.focused() == Some(n) {
                    e.doc.set_focus(None);
                }
                Val::Undefined
            }
            "focused" => Val::from_opt_id(doc!().doc.focused()),
            "is_focusable" => Val::Bool(doc!().doc.is_focusable(id(a, 0)?)),
            "is_text_control" => Val::Bool(doc!().doc.is_text_control(id(a, 0)?)),
            // ---- geometry & style ----
            "rect" => {
                let mut e = doc!();
                e.set_time(self.start.elapsed().as_secs_f64());
                e.ensure_layout();
                rect_val(e.element_rect(id(a, 0)?))
            }
            "layout_size" => {
                let mut e = doc!();
                e.set_time(self.start.elapsed().as_secs_f64());
                e.ensure_layout();
                match e.layout_size(id(a, 0)?) {
                    Some((w, h)) => Val::List(vec![Val::Num(w as f64), Val::Num(h as f64)]),
                    None => Val::Null,
                }
            }
            "scroll_metrics" => {
                let mut e = doc!();
                e.set_time(self.start.elapsed().as_secs_f64());
                e.ensure_layout();
                match e.scroll_metrics(id(a, 0)?) {
                    Some((sw, sh, cw, ch)) => Val::List([sw, sh, cw, ch].iter().map(|v| Val::Num(*v as f64)).collect()),
                    None => Val::Null,
                }
            }
            "get_scroll" => {
                let mut e = doc!();
                e.set_time(self.start.elapsed().as_secs_f64());
                e.ensure_layout();
                let (x, y) = e.get_scroll(id(a, 0)?);
                Val::List(vec![Val::Num(x as f64), Val::Num(y as f64)])
            }
            "set_scroll" => {
                let mut e = doc!();
                e.ensure_layout();
                let n = id(a, 0)?;
                let (cx, cy) = e.get_scroll(n);
                let x = if matches!(a.get(1), Some(Val::Num(_))) { num(a, 1) as f32 } else { cx };
                let y = if matches!(a.get(2), Some(Val::Num(_))) { num(a, 2) as f32 } else { cy };
                e.set_scroll(n, x, y);
                Val::Undefined
            }
            "computed" => {
                let mut e = doc!();
                e.set_time(self.start.elapsed().as_secs_f64());
                e.ensure_layout();
                Val::Str(e.computed_value(id(a, 0)?, &s(a, 1)))
            }
            "hit_test" => {
                let mut e = doc!();
                e.set_time(self.start.elapsed().as_secs_f64());
                e.ensure_layout();
                Val::from_opt_id(e.hit_test(num(a, 0) as f32, num(a, 1) as f32))
            }
            "viewport" => {
                let e = doc!();
                Val::List(vec![Val::Num(e.viewport.0 as f64), Val::Num(e.viewport.1 as f64), Val::Num(e.scale as f64)])
            }
            "media_matches" => {
                let q = s(a, 0);
                let e = doc!();
                let parsed = rendrix_css::parser::MediaQueryList::parse(&q);
                let mut matched = parsed.matches(&e.media_context());
                // The style engine does not track OS accessibility settings; the host owns them.
                let ql = q.to_ascii_lowercase();
                if ql.contains("prefers-reduced-motion") {
                    matched = if ql.contains("reduce") && !ql.contains("no-preference") {
                        self.prefs_reduced_motion
                    } else if ql.contains("no-preference") {
                        !self.prefs_reduced_motion
                    } else {
                        matched
                    };
                }
                if ql.contains("prefers-contrast") {
                    matched = if ql.contains("more") {
                        self.prefs_contrast_more
                    } else if ql.contains("no-preference") || ql.contains("less") {
                        !self.prefs_contrast_more
                    } else {
                        matched
                    };
                }
                Val::Bool(matched)
            }
            "image_info" => match doc!().image_entry(&s(a, 0)) {
                Some(i) => Val::List(vec![Val::Num(i.width as f64), Val::Num(i.height as f64)]),
                None => Val::Null,
            },
            "add_stylesheet" => {
                doc!().add_stylesheet(&s(a, 0));
                Val::Undefined
            }
            // ---- memory ----
            "detach_epoch" => Val::Num(doc!().doc.detach_epoch as f64),
            "take_freed" => {
                let all = {
                    let mut e = doc!();
                    // Sweep now so script-side state for unreachable nodes is dropped promptly.
                    e.doc.collect_garbage();
                    let freed = e.doc.take_freed();
                    let mut all = e.take_freed_for_js();
                    all.extend(freed);
                    all
                };
                // 3D scenes die with their host element (stops future renders + frees memory).
                crate::scene3d::remove_many(crate::canvas_ops::doc_key(self), &all);
                ids(all)
            }
            "retain" => {
                doc!().doc.retain(id(a, 0)?);
                Val::Undefined
            }
            "release" => {
                doc!().doc.release(id(a, 0)?);
                Val::Undefined
            }
            // ---- mutation observation ----
            "observe_mutations" => {
                doc!().doc.set_record_mutations(a.first().is_some_and(|v| v.as_bool()));
                Val::Undefined
            }
            "take_mutations" => {
                let recs = doc!().doc.take_mutations();
                Val::List(
                    recs.into_iter()
                        .map(|r| {
                            Val::List(vec![
                                Val::Str(match r.kind {
                                    MutationKind::ChildList => "childList",
                                    MutationKind::Attributes => "attributes",
                                    MutationKind::CharacterData => "characterData",
                                }
                                .into()),
                                Val::Num(r.target as f64),
                                ids(r.added),
                                ids(r.removed),
                                Val::from_opt_id(r.previous_sibling),
                                Val::from_opt_id(r.next_sibling),
                                r.attribute_name.map(Val::Str).unwrap_or(Val::Null),
                                r.old_value.map(Val::Str).unwrap_or(Val::Null),
                                ids(r.ancestors),
                            ])
                        })
                        .collect(),
                )
            }
            // ---- time ----
            "now" => Val::Num(self.now_ms()),
            "timer_set" => {
                let (tid, delay) = (num(a, 0) as u32, num(a, 1).max(0.0));
                let repeat = a.get(2).is_some_and(|v| v.as_bool());
                self.timers.insert(tid, (self.now_ms() + delay, repeat.then_some(delay.max(1.0))));
                Val::Undefined
            }
            "timer_clear" => {
                self.timers.remove(&(num(a, 0) as u32));
                Val::Undefined
            }
            "raf_request" => {
                self.raf_requested = true;
                Val::Undefined
            }
            "random" => Val::Num(self.rand()),
            // ---- console ----
            "console" => {
                let (level, msg) = (s(a, 0), s(a, 1));
                if self.echo_console {
                    if matches!(level.as_str(), "error" | "warn") {
                        eprintln!("[{level}] {msg}");
                    } else {
                        println!("[{level}] {msg}");
                    }
                }
                self.console.push((level, msg));
                if self.console.len() > 2000 {
                    self.console.drain(..500);
                }
                Val::Undefined
            }
            // ---- storage ----
            "storage_get" => self.storage.area(&s(a, 0)).get(&s(a, 1)).cloned().map(Val::Str).unwrap_or(Val::Null),
            "storage_set" => {
                let area = s(a, 0);
                self.storage.area(&area).insert(s(a, 1), s(a, 2));
                if area != "session" {
                    self.storage.persist();
                }
                Val::Undefined
            }
            "storage_remove" => {
                let area = s(a, 0);
                self.storage.area(&area).remove(&s(a, 1));
                if area != "session" {
                    self.storage.persist();
                }
                Val::Undefined
            }
            "storage_clear" => {
                let area = s(a, 0);
                self.storage.area(&area).clear();
                if area != "session" {
                    self.storage.persist();
                }
                Val::Undefined
            }
            "storage_keys" => Val::List(self.storage.area(&s(a, 0)).keys().cloned().map(Val::Str).collect()),
            // ---- network ----
            "fetch_start" => {
                let headers: Vec<(String, String)> = match a.get(3) {
                    Some(Val::List(l)) => l.chunks(2).filter_map(|c| Some((c.first()?.as_str()?.to_string(), c.get(1)?.as_str()?.to_string()))).collect(),
                    _ => vec![],
                };
                let body = a.get(4).and_then(|v| v.as_str()).and_then(base64_decode);
                self.start_fetch(num(a, 0) as u32, s(a, 1), s(a, 2).to_uppercase(), headers, body);
                Val::Undefined
            }
            "base64_encode_str" => Val::Str(base64_encode(s(a, 0).as_bytes())),
            // ---- clipboard ----
            "clipboard_read" => Val::Str(self.clipboard.clone()),
            "clipboard_write" => {
                self.clipboard = s(a, 0);
                Val::Undefined
            }
            // ---- native dialogs (A2): async, resolved through the completion pipe ----
            "dialog_open" => {
                self.start_dialog(num(a, 0) as u32, false, &s(a, 1));
                Val::Undefined
            }
            "dialog_save" => {
                self.start_dialog(num(a, 0) as u32, true, &s(a, 1));
                Val::Undefined
            }
            "open_external" => {
                Self::open_external(&s(a, 0))?;
                Val::Undefined
            }
            // ---- multi-window requests (A1): drained by the platform shell, never blocking ----
            "window_open" => {
                self.window_open_requests.push(WindowOpenRequest { id: num(a, 0) as u32, url: s(a, 1), target: s(a, 2), features: s(a, 3) });
                Val::Num(num(a, 0))
            }
            "window_close_id" => {
                self.window_close_requests.push(num(a, 0) as u32);
                Val::Undefined
            }
            // ---- WebSocket client (A5): connect/send/close never block the frame loop ----
            "ws_connect" => {
                let protos = match a.get(2) {
                    Some(Val::List(l)) => l.iter().filter_map(|v| v.as_str().map(str::to_string)).collect(),
                    Some(Val::Str(p)) => vec![p.clone()],
                    _ => vec![],
                };
                let origin = a.get(3).and_then(|v| v.as_str()).map(str::to_string);
                self.start_ws(num(a, 0) as u32, s(a, 1), protos, origin);
                Val::Undefined
            }
            "ws_send" => {
                // Payload is always base64 (text is UTF-8 first); `is_text` selects the frame type.
                let is_text = a.get(2).is_some_and(|v| v.as_bool());
                let data = base64_decode(&s(a, 1)).ok_or_else(|| "ws_send: invalid base64 payload".to_string())?;
                self.ws_send_to(num(a, 0) as u32, data, is_text).map_err(|e| e.to_string())?;
                Val::Undefined
            }
            "ws_close" => {
                self.ws_close_socket(num(a, 0) as u32, a.get(1).and_then(|v| v.as_f64()).unwrap_or(1005.0) as u16, s(a, 2));
                Val::Undefined
            }
            // ---- app updates (S1): manifest check → hashed download → staged install ----
            "update_check" => {
                self.start_update_check(num(a, 0) as u32, s(a, 1), s(a, 2));
                Val::Undefined
            }
            "update_download" => {
                self.start_update_download(num(a, 0) as u32, s(a, 1), s(a, 2));
                Val::Undefined
            }
            "update_install" => {
                self.update_install(&s(a, 0))?;
                Val::Undefined
            }
            // ---- accessibility snapshot (A7): JSON for tests; screen readers use the winit bridge ----
            "a11y_snapshot" => {
                {
                    let mut e = self.engine.borrow_mut();
                    e.set_time(self.start.elapsed().as_secs_f64());
                    e.ensure_layout();
                }
                Val::Str(a11y::snapshot_json(self))
            }
            // ---- window ----
            "window_title" => {
                self.window_commands.push(WindowCommand::SetTitle(s(a, 0)));
                Val::Undefined
            }
            "window_close" => {
                self.window_commands.push(WindowCommand::Close);
                Val::Undefined
            }
            "window_resize" => {
                self.window_commands.push(WindowCommand::Resize(num(a, 0) as f32, num(a, 1) as f32));
                Val::Undefined
            }
            "quit" => {
                self.quit_requested = true;
                self.window_commands.push(WindowCommand::Quit);
                Val::Undefined
            }
            // ---- filesystem (permission-gated) ----
            "fs_read_text" => {
                let p = Self::fs_allowed(&self.permissions.fs_read, &s(a, 0)).ok_or_else(|| "filesystem read is not permitted for this path".to_string())?;
                Val::Str(std::fs::read_to_string(p).map_err(|e| e.to_string())?)
            }
            "fs_write_text" => {
                let p = Self::fs_allowed(&self.permissions.fs_write, &s(a, 0)).ok_or_else(|| "filesystem write is not permitted for this path".to_string())?;
                std::fs::write(p, s(a, 1)).map_err(|e| e.to_string())?;
                Val::Undefined
            }
            "fs_exists" => Val::Bool(Self::fs_allowed(&self.permissions.fs_read, &s(a, 0)).is_some_and(|p| p.exists())),
            "fs_read_dir" => {
                let p = Self::fs_allowed(&self.permissions.fs_read, &s(a, 0)).ok_or_else(|| "filesystem read is not permitted for this path".to_string())?;
                let mut names: Vec<String> = std::fs::read_dir(p).map_err(|e| e.to_string())?.filter_map(|e| e.ok()).map(|e| e.file_name().to_string_lossy().into_owned()).collect();
                names.sort();
                Val::List(names.into_iter().map(Val::Str).collect())
            }
            other if other.starts_with("canvas_") => match crate::canvas_ops::canvas_op(self, other, a) {
                Some(r) => r?,
                None => return Err(format!("unknown native operation '{other}'")),
            },
            other if other.starts_with("scene3d_") => match crate::scene3d::scene_op(self, other, a) {
                Some(r) => r?,
                None => return Err(format!("unknown native operation '{other}'")),
            },
            other => return Err(format!("unknown native operation '{other}'")),
        })
    }
}

#[allow(dead_code)]
type _Unused = HashMap<(), ()>;

/// Accessibility tree (A7): an AccessKit snapshot built from the DOM after layout.
///
/// The full-fidelity tree belongs in `rendrix-core` during `Engine::update` (see the migration
/// table in `docs/ENGINE_ASSESSMENT.md` §4 — proposed to the engine owner); this module builds
/// an equivalent snapshot from public engine queries so the platform bridge and the manual
/// screen-reader matrix are unblocked. Bounds are physical pixels (`rect × scale`).
pub mod a11y {
    use super::DomHost;
    use rendrix_dom::NodeId;

    /// Window root id. DOM node ids start at 0, so DOM nodes are offset by one.
    pub fn node_id(dom: NodeId) -> accesskit::NodeId {
        accesskit::NodeId((dom as u64).wrapping_add(1))
    }

    fn role_from_aria(role: &str) -> Option<accesskit::Role> {
        use accesskit::Role as R;
        Some(match role {
            "button" => R::Button,
            "link" => R::Link,
            "checkbox" => R::CheckBox,
            "radio" => R::RadioButton,
            "textbox" => R::TextInput,
            "searchbox" => R::SearchInput,
            "combobox" => R::ComboBox,
            "listbox" => R::ListBox,
            "option" => R::ListBoxOption,
            "heading" => R::Heading,
            "img" => R::Image,
            "presentation" | "none" => R::GenericContainer,
            "dialog" => R::Dialog,
            "alertdialog" => R::AlertDialog,
            "alert" => R::Alert,
            "status" => R::Status,
            "progressbar" => R::ProgressIndicator,
            "slider" => R::Slider,
            "switch" => R::Switch,
            "tab" => R::Tab,
            "tablist" => R::TabList,
            "tabpanel" => R::TabPanel,
            "menu" => R::Menu,
            "menubar" => R::MenuBar,
            "menuitem" => R::MenuItem,
            "menuitemcheckbox" => R::MenuItemCheckBox,
            "menuitemradio" => R::MenuItemRadio,
            "form" => R::Form,
            "group" => R::Group,
            "region" => R::Region,
            "navigation" => R::Navigation,
            "main" => R::Main,
            "complementary" => R::Complementary,
            "banner" => R::Banner,
            "contentinfo" => R::ContentInfo,
            "search" => R::Search,
            "table" => R::Table,
            "row" => R::Row,
            "cell" | "gridcell" => R::Cell,
            "columnheader" => R::ColumnHeader,
            "rowheader" => R::RowHeader,
            "list" => R::List,
            "listitem" => R::ListItem,
            _ => return None,
        })
    }

    fn role_for(tag: &str, input_type: Option<&str>) -> accesskit::Role {
        use accesskit::Role as R;
        match tag {
            "button" => R::Button,
            "a" => R::Link,
            "input" => match input_type.unwrap_or("text") {
                "checkbox" => R::CheckBox,
                "radio" => R::RadioButton,
                "button" | "submit" | "reset" | "image" => R::Button,
                "password" => R::PasswordInput,
                "email" => R::EmailInput,
                "number" => R::NumberInput,
                "url" => R::UrlInput,
                "tel" => R::PhoneNumberInput,
                "search" => R::SearchInput,
                "date" => R::DateInput,
                "time" => R::TimeInput,
                "month" => R::MonthInput,
                "week" => R::WeekInput,
                "datetime-local" => R::DateTimeInput,
                "file" | "color" | "range" | "hidden" => R::GenericContainer,
                _ => R::TextInput,
            },
            "textarea" => R::MultilineTextInput,
            "select" => R::ComboBox,
            "option" => R::MenuListOption,
            "img" => R::Image,
            "label" => R::Label,
            "form" => R::Form,
            "ul" | "ol" | "dl" => R::List,
            "li" | "dt" | "dd" => R::ListItem,
            "table" => R::Table,
            "tr" => R::Row,
            "td" | "th" => R::Cell,
            "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => R::Heading,
            "p" | "pre" => R::Paragraph,
            "blockquote" => R::Blockquote,
            "code" => R::Code,
            "em" => R::Emphasis,
            "strong" => R::Strong,
            "abbr" => R::Abbr,
            "article" => R::Article,
            "aside" => R::Complementary,
            "section" => R::Section,
            "header" => R::Header,
            "footer" => R::Footer,
            "main" => R::Main,
            "nav" => R::Navigation,
            "dialog" => R::Dialog,
            "video" => R::Video,
            "audio" => R::Audio,
            "canvas" => R::Canvas,
            _ => R::GenericContainer,
        }
    }

    fn is_skipped_subtree(tag: &str) -> bool {
        matches!(tag, "head" | "script" | "style" | "template" | "noscript" | "meta" | "link" | "title")
    }

    fn truncate(s: &str, max: usize) -> String {
        if s.len() <= max {
            return s.to_string();
        }
        let mut end = max;
        while !s.is_char_boundary(end) {
            end -= 1;
        }
        s[..end].to_string()
    }

    /// Accessible name: `aria-label` → `aria-labelledby` → `<label>`/`alt`/`title` → text content.
    fn name_for(doc: &rendrix_dom::Document, id: NodeId, tag: &str) -> Option<String> {
        if let Some(label) = doc.get_attr(id, "aria-label").filter(|s| !s.trim().is_empty()) {
            return Some(truncate(label.trim(), 500));
        }
        if let Some(ids) = doc.get_attr(id, "aria-labelledby") {
            let mut parts = Vec::new();
            for part in ids.split_whitespace() {
                if let Some(n) = doc.get_element_by_id(part) {
                    let t = doc.text_content(n);
                    if !t.trim().is_empty() {
                        parts.push(t.trim().to_string());
                    }
                }
            }
            if !parts.is_empty() {
                return Some(truncate(&parts.join(" "), 500));
            }
        }
        // <label for=id> or a wrapping <label>.
        if matches!(tag, "input" | "textarea" | "select" | "button") {
            if let Some(el_id) = doc.get_attr(id, "id") {
                let el_id = el_id.to_string();
                for n in doc.depth_first(doc.root()) {
                    if doc.tag(n) == Some("label") && doc.get_attr(n, "for") == Some(el_id.as_str()) {
                        let t = doc.text_content(n);
                        if !t.trim().is_empty() {
                            return Some(truncate(t.trim(), 500));
                        }
                    }
                }
            }
            let mut p = doc.parent(id);
            while let Some(n) = p {
                if doc.tag(n) == Some("label") {
                    let t = doc.text_content(n);
                    if !t.trim().is_empty() {
                        return Some(truncate(t.trim(), 500));
                    }
                    break;
                }
                p = doc.parent(n);
            }
        }
        if tag == "img" {
            if let Some(alt) = doc.get_attr(id, "alt") {
                return Some(truncate(alt, 500));
            }
        }
        if matches!(tag, "a" | "button" | "h1" | "h2" | "h3" | "h4" | "h5" | "h6" | "label" | "option") {
            let t = doc.text_content(id);
            if !t.trim().is_empty() {
                return Some(truncate(t.trim(), 500));
            }
        }
        if let Some(title) = doc.get_attr(id, "title").filter(|s| !s.trim().is_empty()) {
            return Some(truncate(title.trim(), 500));
        }
        None
    }

    /// Build a full AccessKit tree update for the current document.
    pub fn build_tree(host: &DomHost) -> accesskit::TreeUpdate {
        use accesskit::{Action, Node, NodeId as AId, Rect, Role, Toggled, TreeInfo, TreeUpdate};
        let engine = host.engine.borrow();
        let doc = &engine.doc;
        let scale = engine.scale.max(0.001);
        let mut nodes: Vec<(AId, Node)> = Vec::new();
        let mut kids: std::collections::HashMap<u64, Vec<AId>> = std::collections::HashMap::new();
        // Accesskit ids present in this update; children of anything else hang off the window root.
        let mut included: std::collections::HashSet<u64> = std::collections::HashSet::new();
        // Window root.
        let root_aid = AId(0);
        included.insert(root_aid.0);
        let root_dom = doc.document_element().unwrap_or_else(|| doc.root());
        let mut root = Node::new(Role::Window);
        root.set_label(&host.app_name);
        nodes.push((root_aid, root));

        // `aria-hidden` subtrees and `hidden` elements are excluded; `display:none` (no rect)
        // excludes the element itself but the walk continues (cheap, harmless).
        let mut hidden_subtree: Vec<NodeId> = Vec::new();
        for n in doc.depth_first(root_dom) {
            if hidden_subtree.iter().any(|h| doc.event_path(n).contains(h)) {
                continue;
            }
            let Some(tag) = doc.tag(n).map(str::to_string) else {
                // Text node: expose non-blank text as a run so static content is readable.
                let t = doc.text_content(n);
                if t.trim().is_empty() {
                    continue;
                }
                let aid = node_id(n);
                let mut node = Node::new(Role::TextRun);
                node.set_value(truncate(&t, 2000));
                // Text nodes have no hit rect of their own; expose the nearest element
                // ancestor's bounds (per-character geometry is follow-up work).
                let mut pr = None;
                let mut q = doc.parent(n);
                while let Some(a) = q {
                    pr = engine.element_rect(a);
                    if pr.is_some() {
                        break;
                    }
                    q = doc.parent(a);
                }
                if let Some(r) = pr {
                    node.set_bounds(Rect::new((r[0] * scale) as f64, (r[1] * scale) as f64, ((r[0] + r[2]) * scale) as f64, ((r[1] + r[3]) * scale) as f64));
                }
                // Nearest included ancestor (the direct parent may be skipped, e.g. `hidden`).
                let mut key = u64::MAX;
                let mut p = doc.parent(n);
                while let Some(a) = p {
                    if included.contains(&node_id(a).0) {
                        key = node_id(a).0;
                        break;
                    }
                    p = doc.parent(a);
                }
                kids.entry(key).or_default().push(aid);
                included.insert(aid.0);
                nodes.push((aid, node));
                continue;
            };
            if is_skipped_subtree(&tag) {
                hidden_subtree.push(n);
                continue;
            }
            if doc.has_attr(n, "hidden") || doc.get_attr(n, "aria-hidden") == Some("true") {
                hidden_subtree.push(n);
                continue;
            }
            let input_type = if tag == "input" { doc.get_attr(n, "type").map(|t| t.to_ascii_lowercase()) } else { None };
            let mut role = match doc.get_attr(n, "role") {
                Some(r) => role_from_aria(&r.to_ascii_lowercase()).unwrap_or_else(|| role_for(&tag, input_type.as_deref())),
                None => role_for(&tag, input_type.as_deref()),
            };
            if tag == "a" && doc.get_attr(n, "href").is_none() && role == Role::Link {
                role = Role::GenericContainer;
            }
            let aid = node_id(n);
            let mut node = Node::new(role);
            node.set_html_tag(&tag);
            if let Some(id_attr) = doc.get_attr(n, "id") {
                node.set_html_id(id_attr);
                node.set_author_id(id_attr);
            }
            if let Some(name) = name_for(doc, n, &tag) {
                node.set_label(name);
            }
            if tag.starts_with('h') && tag.len() == 2 {
                if let Some(level) = tag[1..].parse::<usize>().ok().filter(|l| (1..=6).contains(l)) {
                    node.set_level(level - 1);
                }
            }
            if matches!(role, Role::TextInput | Role::MultilineTextInput | Role::PasswordInput | Role::EmailInput | Role::NumberInput | Role::UrlInput | Role::SearchInput) {
                node.set_value(truncate(&doc.value(n), 2000));
                if let Some(ph) = doc.get_attr(n, "placeholder").filter(|s| !s.is_empty()) {
                    node.set_placeholder(ph);
                }
                node.add_action(Action::SetValue);
            }
            if tag == "a" {
                if let Some(href) = doc.get_attr(n, "href") {
                    node.set_url(href);
                }
            }
            if matches!(role, Role::CheckBox | Role::RadioButton) {
                node.set_toggled(Toggled::from(doc.is_checked(n)));
            }
            if tag == "option" {
                node.set_selected(doc.has_attr(n, "selected"));
            }
            if doc.get_attr(n, "disabled").is_some() {
                node.set_disabled();
            }
            if doc.get_attr(n, "required").is_some() || doc.get_attr(n, "aria-required") == Some("true") {
                node.set_required();
            }
            if doc.is_focusable(n) {
                node.add_action(Action::Focus);
            }
            if matches!(role, Role::Button | Role::Link | Role::CheckBox | Role::RadioButton | Role::MenuListOption) {
                node.add_action(Action::Click);
            }
            if let Some(r) = engine.element_rect(n) {
                node.set_bounds(Rect::new((r[0] * scale) as f64, (r[1] * scale) as f64, ((r[0] + r[2]) * scale) as f64, ((r[1] + r[3]) * scale) as f64));
            }
            if let Some(p) = doc.parent(n) {
                let mut key = u64::MAX;
                let mut a = Some(p);
                while let Some(x) = a {
                    if included.contains(&node_id(x).0) {
                        key = node_id(x).0;
                        break;
                    }
                    a = doc.parent(x);
                }
                kids.entry(key).or_default().push(aid);
            } else {
                kids.entry(u64::MAX).or_default().push(aid);
            }
            included.insert(aid.0);
            nodes.push((aid, node));
        }
        // Attach children lists (document order preserved by the depth-first walk).
        for (aid, node) in nodes.iter_mut() {
            if let Some(k) = kids.remove(&aid.0) {
                node.set_children(k);
            }
        }
        // Top-level nodes hang off the window root.
        if let Some(top) = kids.remove(&u64::MAX) {
            if let Some((_, node)) = nodes.iter_mut().find(|(id, _)| *id == root_aid) {
                let mut k: Vec<AId> = node.children().to_vec();
                k.extend(top);
                node.set_children(k);
            }
        }
        let focus = doc.focused().map(node_id).unwrap_or(root_aid);
        let mut tree = TreeInfo::new(root_aid);
        tree.toolkit_name = Some("Rendrix".to_string());
        tree.toolkit_version = Some(env!("CARGO_PKG_VERSION").to_string());
        TreeUpdate { nodes, tree: Some(tree), tree_id: accesskit::TreeId::ROOT, focus }
    }

    /// Compact JSON snapshot of the tree (`[{id, role, name}]`) for headless tests and the
    /// `a11y_snapshot` host op. Not the screen-reader path — that uses `build_tree`.
    pub fn snapshot_json(host: &DomHost) -> String {
        let update = build_tree(host);
        let mut names: std::collections::HashMap<u64, String> = std::collections::HashMap::new();
        let mut roles: std::collections::HashMap<u64, String> = std::collections::HashMap::new();
        for (id, node) in &update.nodes {
            roles.insert(id.0, format!("{:?}", node.role()));
            if let Some(label) = node.label() {
                names.insert(id.0, label.to_string());
            } else if let Some(value) = node.value() {
                if !value.trim().is_empty() {
                    names.insert(id.0, value.chars().take(60).collect());
                }
            }
        }
        let items: Vec<serde_json::Value> = update
            .nodes
            .iter()
            .map(|(id, node)| {
                serde_json::json!({
                    "id": id.0,
                    "role": roles.get(&id.0).cloned().unwrap_or_default(),
                    "name": names.get(&id.0).cloned().unwrap_or_default(),
                    "children": node.children().iter().map(|c| c.0).collect::<Vec<_>>(),
                })
            })
            .collect();
        serde_json::to_string(&items).unwrap_or_default()
    }
}

#[cfg(test)]
mod host_tests {
    use super::*;
    use crate::engine_trait::Host;

    fn host() -> DomHost {
        let doc = rendrix_dom::Document::parse("<html><body><p>x</p></body></html>");
        let engine = Engine::with_document(doc, Box::new(rendrix_core::MapAssets::default()));
        let mut h = DomHost::new(std::rc::Rc::new(RefCell::new(engine)), "test-app", Permissions::allow_all());
        h.echo_console = false;
        h.disable_storage_persistence();
        h
    }

    #[test]
    fn sha256_matches_known_vectors() {
        let mut h = Sha256::new();
        h.update(b"abc");
        let hex: String = h.finalize().iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(hex, "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
        let mut h = Sha256::new();
        h.update(b"");
        let hex: String = h.finalize().iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(hex, "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855");
    }

    #[test]
    fn version_compare_orders_dotted_versions() {
        assert_eq!(version_compare("1.10.0", "1.9.0"), std::cmp::Ordering::Greater);
        assert_eq!(version_compare("2.0", "2.0.0"), std::cmp::Ordering::Less);
        assert_eq!(update_available_json(r#"{"version":"1.2.0","url":"https://x/y","sha256":"abc"}"#, "1.1.9").contains(r#""updateAvailable":true"#), true);
        assert!(update_available_json(r#"{"version":"1.1.9","url":"https://x/y"}"#, "1.1.9").contains(r#""updateAvailable":false"#));
        assert!(update_available_json("not json", "1.0").contains("error"));
        assert!(update_available_json(r#"{"version":"1.0"}"#, "0.9").contains("error"));
    }

    #[test]
    fn cookie_jar_stores_sends_and_deletes() {
        let mut h = host();
        h.apply_cookies("example.com", &["a=1; Path=/".to_string(), "b=2; Secure".to_string()]);
        let jar = h.cookie_jar.get("example.com").unwrap();
        assert_eq!(jar.get("a").map(String::as_str), Some("1"));
        h.apply_cookies("example.com", &["a=gone; Max-Age=0".to_string()]);
        assert!(!h.cookie_jar.get("example.com").unwrap().contains_key("a"));
        assert_eq!(url_host("https://example.com:8080/p?q=1"), Some("example.com".into()));
        assert_eq!(url_host("http://user@[::1]:80/"), Some("::1".into()));
        assert_eq!(url_host("data:text/plain,hi"), None);
    }

    #[test]
    fn dialogs_resolve_from_the_mock_without_os_ui() {
        let mut h = host();
        h.set_dialog_mock(Some(Some(vec!["/tmp/a.txt".to_string()])));
        h.call("dialog_open", &[Val::Num(7.0), Val::Str("{}".into())]).unwrap();
        let done = h.poll_fetches();
        assert_eq!(done.len(), 1);
        assert_eq!((done[0].id, done[0].status_text.as_str()), (7, TAG_DIALOG));
        assert_eq!(String::from_utf8(done[0].body.clone()).unwrap(), r#"["/tmp/a.txt"]"#);
        h.set_dialog_mock(Some(None));
        h.call("dialog_save", &[Val::Num(8.0), Val::Str("{}".into())]).unwrap();
        let done = h.poll_fetches();
        assert_eq!(String::from_utf8(done[0].body.clone()).unwrap(), "null");
    }

    #[test]
    fn open_external_rejects_non_web_schemes() {
        assert!(DomHost::open_external("file:///etc/passwd").is_err());
        assert!(DomHost::open_external("javascript:alert(1)").is_err());
    }

    #[test]
    fn websocket_echo_over_loopback() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut ws = tungstenite::accept(stream).unwrap();
            loop {
                match ws.read() {
                    Ok(tungstenite::Message::Text(t)) => {
                        if ws.send(tungstenite::Message::Text(t)).is_err() {
                            break;
                        }
                    }
                    Ok(tungstenite::Message::Close(_)) => break,
                    Err(_) => break,
                    _ => {}
                }
            }
        });
        let mut h = host();
        // Localhost works without the network permission (loopback test exception).
        h.permissions.network = false;
        h.call("ws_connect", &[Val::Num(1.0), Val::Str(format!("ws://127.0.0.1:{port}/")), Val::List(vec![])]).unwrap();
        let mut seen_open = false;
        let mut seen_echo = false;
        for _ in 0..500 {
            for r in h.poll_fetches() {
                match r.status_text.as_str() {
                    TAG_WS_OPEN => {
                        seen_open = true;
                        h.call("ws_send", &[Val::Num(1.0), Val::Str(crate::host::base64_encode(b"hello")), Val::Bool(true)]).unwrap();
                    }                    TAG_WS_MESSAGE => {
                        assert_eq!((r.status, String::from_utf8(r.body.clone()).unwrap()), (1, "hello".to_string()));
                        seen_echo = true;
                        h.call("ws_close", &[Val::Num(1.0), Val::Num(1000.0), Val::Str(String::new())]).unwrap();
                    }
                    TAG_WS_CLOSE => {
                        assert!(seen_echo);
                        return;
                    }
                    other => panic!("unexpected event {other}: {}", String::from_utf8_lossy(&r.body)),
                }
            }
            if seen_open && !seen_echo {
                std::thread::sleep(std::time::Duration::from_millis(2));
            } else if !seen_open {
                std::thread::sleep(std::time::Duration::from_millis(2));
            } else {
                // Close handshake needs a worker turn (~50 ms read cadence).
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
        }
        panic!("websocket echo did not complete (open={seen_open}, echo={seen_echo})");
    }

    #[test]
    fn a11y_tree_has_roles_names_and_focus() {
        let h = host();
        {
            let mut e = h.engine.borrow_mut();
            e.set_time(1.0);
            e.ensure_layout();
        }
        let update = a11y::build_tree(&h);
        assert!(update.tree.is_some());
        let roles: Vec<String> = update.nodes.iter().map(|(_, n)| format!("{:?}", n.role())).collect();
        assert!(roles.contains(&"Window".to_string()), "{roles:?}");
        assert!(roles.contains(&"TextRun".to_string()), "{roles:?}");
        let json: serde_json::Value = serde_json::from_str(&a11y::snapshot_json(&h)).unwrap();
        assert!(json.as_array().unwrap().iter().any(|n| n["role"] == "Window"));
        // Focus lands on the window root when nothing is focused.
        assert_eq!(update.focus.0, 0);
    }
}
