//! State-preserving hot reload (D2): swap module sources and re-run entry scripts in a
//! live [`Session`](crate::session::Session) without restarting the process.
//!
//! Contract of [`apply_module_update`]:
//! - `changed` must be the COMPLETE post-update module set (what `buildApp` emits as
//!   `bundle.modules`: every module the app imports, keyed by bundle path). Entries that are
//!   absent are treated as deleted: the update fails closed during validation instead of
//!   running with a half-present graph.
//! - Validation runs first in a scratch [`Session`](crate::session::Session) built from the
//!   live document's serialized HTML. Only module-evaluation errors (syntax errors,
//!   unresolvable imports, synchronous top-level throws) fail validation; async work is never
//!   awaited there, so logic bugs still surface at commit time like any normal run.
//! - Commit builds a fresh `JsRuntime` on the SAME live `Engine` (the DOM, styles, layout and
//!   scroll state are never discarded) and swaps it in only after every entry script succeeds.
//!   On failure the previous `JsRuntime` stays in place, so script state is last-good.
//! - DOM/JS-heap preservation is script-cooperative: a fresh JS heap means module-level `let`
//!   bindings restart and timers are re-armed by the re-run scripts. Scripts that rebuild the
//!   DOM idempotently (conditional init, `render()` replacing container children) observe full
//!   state preservation (focus, input values, scroll); scripts that unconditionally rewrite the
//!   document do not.
//!
//! Known limits (see also the crate-level docs the main agent maintains):
//! - Bundler source maps are not carried across the swap (no public accessor exposes them), so
//!   after a hot update error stacks reference compiled bundle paths until the next restart.
//! - The scratch validation bundle carries no assets, so asset-dependent top-level code is not
//!   exercised there; it runs for real at commit time.
//! - A commit that passes validation but throws live (nondeterministic code) can leave partial
//!   DOM mutations behind while the JS heap stays last-good.

use crate::bundle::{AppBundle, FORMAT};
use crate::session::{MemoryClipboard, Session};
use rendrix_js::JsRuntime;
use std::collections::BTreeMap;

fn normalize(p: &str) -> String {
    p.trim_start_matches("./")
        .trim_start_matches('/')
        .split(['?', '#'])
        .next()
        .unwrap_or("")
        .to_string()
}

/// One entry script in document order: (bundle key for modules / label, is_module, inline_source).
/// Mirrors `Session::run_document_scripts` (which lives in a file this module may not edit), so
/// hot-reloaded code runs exactly the scripts a fresh start would run.
fn entry_scripts(session: &Session) -> Vec<(String, bool, Option<String>)> {
    let e = session.engine.borrow();
    let doc = &e.doc;
    let mut out = Vec::new();
    let mut inline = 0;
    for n in doc.get_elements_by_tag_name(doc.root(), "script") {
        let src = doc.get_attr(n, "src").map(str::to_string);
        let text = doc.text_content(n);
        let is_module = doc
            .get_attr(n, "type")
            .is_some_and(|t| t.eq_ignore_ascii_case("module"));
        match (&src, is_module) {
            (Some(s), true) => out.push((format!("module:{s}"), true, None)),
            (None, true) => {
                inline += 1;
                out.push((format!("inline-module-{inline}"), true, Some(text)));
            }
            (Some(s), false) => out.push((format!("classic:{s}"), false, Some(s.clone()))),
            (None, false) => {
                if !text.trim().is_empty() {
                    inline += 1;
                    out.push((format!("inline-classic-{inline}"), false, Some(text)));
                }
            }
        }
    }
    out
}

/// Run the document's entry scripts against `modules`. Returns the entry labels that ran.
fn run_entries(
    session: &mut Session,
    modules: &BTreeMap<String, String>,
) -> Result<Vec<String>, String> {
    for (path, src) in modules {
        session.js.add_module(path, src.clone());
    }
    let scripts = entry_scripts(session);
    let mut ran = Vec::new();
    let mut inline = 0;
    for (label, is_module, payload) in &scripts {
        if *is_module {
            if label.starts_with("module:") {
                let src = label.strip_prefix("module:").unwrap();
                let key = normalize(src);
                let name = [key.clone(), format!("{key}.js")]
                    .into_iter()
                    .find(|k| modules.contains_key(k))
                    .ok_or_else(|| {
                        format!("module script '{src}' is not in the updated modules")
                    })?;
                session
                    .js
                    .run_module(&name)
                    .map_err(|e| format!("{src}: {e}"))?;
            } else {
                inline += 1;
                let name = format!("inline-{inline}.js");
                let code = payload.as_deref().unwrap_or("");
                session.js.add_module(&name, code.to_string());
                session
                    .js
                    .run_module(&name)
                    .map_err(|e| format!("{label}: {e}"))?;
            }
            ran.push(label.clone());
        } else if label.starts_with("classic:") {
            let src = label.strip_prefix("classic:").unwrap();
            let key = normalize(src);
            let code = modules
                .get(&key)
                .cloned()
                .or_else(|| {
                    session
                        .engine
                        .borrow()
                        .read_asset(&key)
                        .map(|b| String::from_utf8_lossy(&b).into_owned())
                })
                .ok_or_else(|| format!("script '{src}' is not in the updated modules"))?;
            session
                .js
                .run_script(src, &code)
                .map_err(|e| format!("{src}: {e}"))?;
            ran.push(label.clone());
        } else {
            inline += 1;
            let code = payload.as_deref().unwrap_or("");
            if !code.trim().is_empty() {
                let name = format!("inline-script-{inline}");
                session
                    .js
                    .run_script(&name, code)
                    .map_err(|e| format!("{label}: {e}"))?;
                ran.push(label.clone());
            }
        }
    }
    session.js.boot();
    Ok(ran)
}

/// Serialize the live document for the scratch validation session.
fn live_html(session: &Session) -> String {
    let e = session.engine.borrow();
    match e.doc.document_element() {
        Some(html) => e.doc.outer_html(html),
        None => e.doc.inner_html(e.doc.root()),
    }
}

/// Swap the module sources of a live session and re-run its entry scripts, preserving the DOM.
///
/// See the module docs for the contract. Returns the entry labels that were re-run.
/// On `Err` the session is untouched (previous scripts keep working); the `Err` string carries
/// the failing module and its stack.
pub fn apply_module_update(
    session: &mut Session,
    changed: BTreeMap<String, String>,
) -> Result<Vec<String>, String> {
    if changed.is_empty() {
        return Err(
            "apply_module_update: empty module set (pass the full bundle.modules map)".into(),
        );
    }
    // Phase 1 — validate in a scratch session built from the live document. This never touches
    // the live engine or the live JS runtime, so a broken update fails here with last-good intact.
    let html = live_html(session);
    let viewport = session.engine.borrow().viewport;
    let scale = session.engine.borrow().scale;
    let perms = session.js.host.borrow().permissions.clone();
    let echo = session.js.host.borrow().echo_console;
    let scratch_bundle = AppBundle {
        format: FORMAT.into(),
        name: session.bundle_name.clone(),
        title: String::new(),
        width: viewport.0 as u32,
        height: viewport.1 as u32,
        html,
        modules: changed.clone(),
        source_maps: BTreeMap::new(),
        assets: BTreeMap::new(),
        permissions: crate::bundle::PermissionConfig {
            network: false,
            fs_read: perms
                .fs_read
                .iter()
                .map(|p| p.to_string_lossy().into_owned())
                .collect(),
            fs_write: perms
                .fs_write
                .iter()
                .map(|p| p.to_string_lossy().into_owned())
                .collect(),
        },
    };
    // Constructing the scratch session runs the updated entries against a copy of the live
    // document: any module-evaluation error rejects the update before the live side is touched.
    let mut scratch = Session::new(
        &scratch_bundle,
        Box::new(MemoryClipboard::default()),
        (viewport.0, viewport.1, scale),
    )
    .map_err(|e| format!("hot reload rejected (live session untouched): {e}"))?;
    scratch.js.host.borrow_mut().echo_console = false;
    scratch.frame();
    drop(scratch);
    drop(scratch_bundle);

    // Phase 2 — commit: fresh JS heap on the SAME live engine (DOM preserved by construction),
    // swapped in only after every entry succeeds.
    let live_perms = session.js.host.borrow().permissions.clone();
    let mut fresh = JsRuntime::new(
        session.engine.clone(),
        &session.bundle_name.clone(),
        live_perms,
    )
    .map_err(|e| format!("hot reload: runtime reset failed: {e}"))?;
    fresh.host.borrow_mut().echo_console = echo;
    std::mem::swap(&mut session.js, &mut fresh);
    match run_entries(session, &changed) {
        Ok(ran) => {
            drop(fresh);
            session.frame();
            Ok(ran)
        }
        Err(e) => {
            // Restore the previous runtime: script state is last-good. DOM mutations the failed
            // entries made before throwing may persist (documented limitation).
            std::mem::swap(&mut session.js, &mut fresh);
            drop(fresh);
            Err(format!(
                "hot reload commit failed (previous scripts restored): {e}"
            ))
        }
    }
}
