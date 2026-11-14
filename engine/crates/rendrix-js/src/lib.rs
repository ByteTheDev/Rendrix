//! Rendrix JavaScript runtime: an engine-neutral host interface, a QuickJS backend, the DOM and
//! Web API prelude, and the event-loop glue the platform layer drives.

pub mod engine_trait;
pub mod host;
pub mod quickjs;
pub mod runtime;
pub mod sourcemap;
pub mod canvas_ops;

pub use engine_trait::{Host, JsEngine, JsError, Val};
pub use host::{DomHost, Permissions, WindowCommand, WindowOpenRequest};
pub use runtime::{JsRuntime, TickInfo, WorkerEvent, WorkerRegistry, WORKER_MAX_PER_TICK};

#[cfg(test)]
mod tests;
