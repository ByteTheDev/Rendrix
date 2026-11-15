//! The browser's JavaScript engine as a Rendrix [`JsEngine`].
//!
//! The Web API surface (DOM, events, timers, fetch, …) is the same JavaScript prelude the native
//! runtime ships; it reaches Rust through the single `__native(op, ...args)` function, which this
//! backend routes to the [`Host`]. Browser JIT performance, no engine in the wasm binary.
//!
//! Differences from the QuickJS backend, all inherent to a browser:
//! * microtasks cannot be drained on demand (`run_jobs` is a no-op; they run when control returns);
//! * ES modules load asynchronously (see `ModuleLoader` in `capture.js`); `boot` is ordered after them;
//! * no execution-budget interrupt (a runaway script hangs the worker, not the page).

use rendrix_js::{Host, JsEngine, JsError, Val};
use std::cell::RefCell;
use std::rc::Rc;
use wasm_bindgen::prelude::*;

#[wasm_bindgen(module = "/src/capture.js")]
extern "C" {
    #[wasm_bindgen(js_name = consoleLog)]
    pub fn console_log(level: &str, msg: &str);
    #[wasm_bindgen(js_name = randomF64)]
    pub fn random_f64() -> f64;
    #[wasm_bindgen(js_name = makeNative)]
    fn make_native(call: &JsValue) -> js_sys::Function;
    #[wasm_bindgen(js_name = installRejectionListener)]
    fn install_rejection_listener(cb: &JsValue);
    #[wasm_bindgen(js_name = evalGlobal, catch)]
    fn eval_global(source: &str) -> Result<(), JsValue>;
    #[wasm_bindgen(js_name = callGlobal, catch)]
    fn call_global_js(name: &str, args: &js_sys::Array) -> Result<JsValue, JsValue>;
    #[wasm_bindgen(js_name = errorInfo)]
    fn error_info(e: &JsValue) -> js_sys::Array;
    #[wasm_bindgen(js_name = doFetch)]
    pub fn do_fetch(url: &str, method: &str, headers: &js_sys::Array, body: &JsValue) -> js_sys::Promise;

    pub type ModuleLoader;
    #[wasm_bindgen(constructor)]
    fn new() -> ModuleLoader;
    #[wasm_bindgen(method)]
    fn add(this: &ModuleLoader, name: &str, source: &str);
    #[wasm_bindgen(method, js_name = enqueueModule)]
    fn enqueue_module(this: &ModuleLoader, name: &str);
    #[wasm_bindgen(method, js_name = enqueueCall)]
    fn enqueue_call(this: &ModuleLoader, name: &str);
    #[wasm_bindgen(method, getter)]
    fn pending(this: &ModuleLoader) -> u32;
    #[wasm_bindgen(method, js_name = takeErrors)]
    fn take_errors(this: &ModuleLoader) -> js_sys::Array;
}

fn to_val(v: &JsValue) -> Val {
    if v.is_undefined() {
        Val::Undefined
    } else if v.is_null() {
        Val::Null
    } else if let Some(b) = v.as_bool() {
        Val::Bool(b)
    } else if let Some(n) = v.as_f64() {
        Val::Num(n)
    } else if let Some(s) = v.as_string() {
        Val::Str(s)
    } else if js_sys::Array::is_array(v) {
        Val::List(js_sys::Array::from(v).iter().map(|x| to_val(&x)).collect())
    } else {
        Val::Undefined
    }
}

fn from_val(v: &Val) -> JsValue {
    match v {
        Val::Undefined => JsValue::UNDEFINED,
        Val::Null => JsValue::NULL,
        Val::Bool(b) => JsValue::from_bool(*b),
        Val::Num(n) => JsValue::from_f64(*n),
        Val::Str(s) => JsValue::from_str(s),
        Val::List(items) => items.iter().map(from_val).collect::<js_sys::Array>().into(),
    }
}

fn js_error(e: &JsValue) -> JsError {
    let info = error_info(e);
    let message = info.get(0).as_string().unwrap_or_default();
    let stack = info.get(1).as_string().filter(|s| !s.is_empty());
    JsError { message, stack }
}

pub struct BrowserEngine {
    loader: ModuleLoader,
    rejections: Rc<RefCell<Vec<String>>>,
    // Kept alive for as long as the engine: JS holds the functions built from these.
    _native: Closure<dyn FnMut(js_sys::Array) -> Result<JsValue, JsValue>>,
    _on_rejection: Closure<dyn FnMut(String)>,
}

impl BrowserEngine {
    pub fn new(host: Rc<RefCell<dyn Host>>) -> Result<BrowserEngine, JsError> {
        let native = Closure::wrap(Box::new(move |args: js_sys::Array| -> Result<JsValue, JsValue> {
            let op = args.get(0).as_string().unwrap_or_default();
            let vals: Vec<Val> = (1..args.length()).map(|i| to_val(&args.get(i))).collect();
            let r = host.borrow_mut().call(&op, &vals);
            match r {
                Ok(v) => Ok(from_val(&v)),
                Err(msg) => Err(js_sys::Error::new(&msg).into()),
            }
        }) as Box<dyn FnMut(js_sys::Array) -> Result<JsValue, JsValue>>);
        let f = make_native(native.as_ref());
        js_sys::Reflect::set(&js_sys::global(), &JsValue::from_str("__native"), &f).map_err(|e| js_error(&e))?;

        let rejections: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(Vec::new()));
        let sink = rejections.clone();
        let on_rejection = Closure::wrap(Box::new(move |text: String| sink.borrow_mut().push(text)) as Box<dyn FnMut(String)>);
        install_rejection_listener(on_rejection.as_ref());
        Ok(BrowserEngine { loader: ModuleLoader::new(), rejections, _native: native, _on_rejection: on_rejection })
    }

    /// Module scripts queued by the page have all finished (or failed).
    pub fn modules_idle(&self) -> bool {
        self.loader.pending() == 0
    }
}

impl JsEngine for BrowserEngine {
    fn name(&self) -> &'static str {
        "browser"
    }

    fn add_module(&mut self, name: &str, source: String) {
        self.loader.add(name, &source);
    }

    fn eval_script(&mut self, name: &str, source: &str) -> Result<(), JsError> {
        let src = format!("{source}\n//# sourceURL=rendrix:///{name}");
        eval_global(&src).map_err(|e| {
            let mut err = js_error(&e);
            err.message = format!("{name}: {}", err.message);
            err
        })
    }

    fn eval_module(&mut self, name: &str) -> Result<(), JsError> {
        self.loader.enqueue_module(name);
        Ok(())
    }

    fn call_global(&mut self, func: &str, args: Vec<Val>) -> Result<Val, JsError> {
        // The document's module scripts load asynchronously; `boot` (DOMContentLoaded/load) must
        // run after them, as it does in a page.
        if func == "__rendrix_boot" && !self.modules_idle() {
            self.loader.enqueue_call(func);
            return Ok(Val::Undefined);
        }
        let arr: js_sys::Array = args.iter().map(from_val).collect();
        match call_global_js(func, &arr) {
            Ok(v) => Ok(to_val(&v)),
            Err(e) => Err(js_error(&e)),
        }
    }

    fn run_jobs(&mut self) -> Result<(), JsError> {
        Ok(())
    }

    fn memory_usage(&self) -> usize {
        0
    }

    fn collect_garbage(&mut self) {}

    fn take_rejections(&mut self) -> Vec<String> {
        let mut out = std::mem::take(&mut *self.rejections.borrow_mut());
        out.extend(self.loader.take_errors().iter().filter_map(|v| v.as_string()));
        out
    }
}
