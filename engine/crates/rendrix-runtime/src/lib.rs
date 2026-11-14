//! Rendrix runtime: loads an application bundle, owns the engine and JavaScript runtime, and drives
//! them from a native window (or headlessly).

pub mod bench;
pub mod bundle;
pub mod headless;
pub mod hot_reload; // D2: apply_module_update
pub mod inspector; // D3: snapshot
pub mod session;
pub mod window;

pub use bundle::AppBundle;
pub use session::Session;
