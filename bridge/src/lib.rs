//! ThatsMatter bridge library: catalog, Matter backend trait, and loopback IPC.

pub mod catalog;
pub mod config;
pub mod ipc;
pub mod matter;
pub mod state;

pub use config::{BackendKind, Config};
pub use state::AppState;
