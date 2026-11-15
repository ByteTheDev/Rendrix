//! Rendrix runtime: loads an application bundle, owns the engine and JavaScript runtime, and drives
//! them from a native window (or headlessly).
//!
//! `session` is platform-independent and also compiles to wasm32 (driven by `rendrix-web`); the
//! winit window shell, the headless GPU driver and desktop gamepads are native-only.

#[cfg(not(target_arch = "wasm32"))]
pub mod bench;
#[cfg(not(any(target_os = "android", target_os = "ios", target_arch = "wasm32")))]
pub mod gamepad;
pub mod bundle;
#[cfg(not(target_arch = "wasm32"))]
pub mod headless;
#[cfg(not(target_arch = "wasm32"))]
pub mod hot_reload; // D2: apply_module_update
pub mod inspector; // D3: snapshot
pub mod session;
#[cfg(not(target_arch = "wasm32"))]
pub mod window;
#[cfg(target_os = "android")]
pub mod android;

pub use bundle::AppBundle;
pub use session::Session;
