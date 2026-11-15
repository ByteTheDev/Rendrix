//! Dedicated Workers on targets without OS threads (wasm32).
//!
//! The `Worker` prelude class still exists so feature detection and construction work, but a spawn
//! is answered with an `error` event followed by `close`. Browser-backed workers are follow-up work.

use std::collections::{HashMap, VecDeque};

pub const WORKER_MAX_PER_TICK: usize = 16;

#[derive(Debug, Clone, PartialEq)]
pub enum WorkerEvent {
    Message { worker: u32, json: String },
    Error { worker: u32, message: String },
    Console { worker: u32, level: String, text: String },
    Closed { worker: u32 },
}

impl WorkerEvent {
    pub fn worker(&self) -> u32 {
        match self {
            WorkerEvent::Message { worker, .. } | WorkerEvent::Error { worker, .. } | WorkerEvent::Console { worker, .. } | WorkerEvent::Closed { worker } => *worker,
        }
    }
}

#[derive(Default)]
pub struct WorkerRegistry {
    queue: VecDeque<WorkerEvent>,
}

impl WorkerRegistry {
    pub fn new() -> WorkerRegistry {
        WorkerRegistry::default()
    }
    pub fn set_modules(&mut self, _modules: HashMap<String, String>) {}
    pub fn set_network(&mut self, _allow: bool) {}
    pub fn spawn(&mut self, id: u32, _url: &str, _kind: &str) {
        self.queue.push_back(WorkerEvent::Error { worker: id, message: "Workers are not supported on the web target yet".into() });
        self.queue.push_back(WorkerEvent::Closed { worker: id });
    }
    pub fn post(&mut self, _id: u32, _json: &str) {}
    pub fn terminate(&mut self, _id: u32) {}
    pub fn note_closed(&mut self, _id: u32) {}
    pub fn drain_capped(&mut self, cap: usize) -> (Vec<WorkerEvent>, bool) {
        let n = cap.min(self.queue.len());
        let out: Vec<WorkerEvent> = self.queue.drain(..n).collect();
        (out, !self.queue.is_empty())
    }
    pub fn pending_delivery(&self) -> bool {
        !self.queue.is_empty()
    }
    pub fn alive(&self) -> bool {
        false
    }
    pub fn shutdown(&mut self) {}
}
