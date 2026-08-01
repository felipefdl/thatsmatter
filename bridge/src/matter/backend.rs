//! Matter stack boundary used by the IPC control plane.

use async_trait::async_trait;
use uuid::Uuid;

use crate::catalog::{CommandRequest, Export, HaStateValue, PairingMaterial};
use crate::config::BackendKind;

/// Abstraction over a Matter protocol implementation.
///
/// The control plane never talks to clusters directly; it goes through this trait
/// so `DevMatterBackend` and a future `rs-matter` backend share the same IPC surface.
#[async_trait]
pub trait MatterBackend: Send + Sync {
  /// Backend kind reported on `/status`.
  fn kind(&self) -> BackendKind;

  /// Start the Matter stack (or mark the dev backend running).
  async fn start(&self) -> anyhow::Result<()>;

  /// Whether the stack is considered running.
  async fn is_running(&self) -> bool;

  /// Whether commissioning / pairing window is open.
  async fn pairing_open(&self) -> bool;

  /// Open the basic commissioning window for `timeout_secs` (clamped 180..=900).
  async fn open_pairing_window(&self, timeout_secs: u16) -> anyhow::Result<()>;

  /// Close any window this bridge opened.
  async fn close_pairing_window(&self) -> anyhow::Result<()>;

  /// Number of commissioned fabrics currently known to the backend.
  async fn commissioned_fabrics(&self) -> u8;

  /// Push the current export set into the Matter node (dynamic endpoints).
  async fn set_exports(&self, exports: &[Export]) -> anyhow::Result<()>;

  /// Apply HA entity state for a specific export (updates cluster attributes).
  async fn apply_state(&self, export_id: Uuid, states: &[HaStateValue]) -> anyhow::Result<u32>;

  /// Drain pending commands originated from the Matter side (controller → HA).
  async fn take_commands(&self) -> Vec<CommandRequest>;

  /// Current pairing material (setup code + QR payload).
  async fn pairing_info(&self) -> PairingMaterial;

  /// Optional last error string for `/status`.
  async fn status_error(&self) -> Option<String>;
}

/// Default open-window timeout when the client omits `timeout_secs`.
pub const PAIRING_TIMEOUT_DEFAULT_SECS: u16 = 300;
/// Spec minimum for a basic commissioning window (`rs-matter` rejects below this).
pub const PAIRING_TIMEOUT_MIN_SECS: u16 = 180;
/// Spec maximum for a basic commissioning window.
pub const PAIRING_TIMEOUT_MAX_SECS: u16 = 900;
/// Startup window when the node has no fabrics yet (matches stack `startup`).
pub const STARTUP_PAIRING_TIMEOUT_SECS: u64 = 900;

/// Clamp a requested pairing-window timeout into the Matter 180..=900 range.
pub fn clamp_pairing_timeout(timeout_secs: u16) -> u16 {
  timeout_secs.clamp(PAIRING_TIMEOUT_MIN_SECS, PAIRING_TIMEOUT_MAX_SECS)
}

/// Unix epoch seconds for window-deadline atomics.
pub fn epoch_secs() -> u64 {
  use std::time::{SystemTime, UNIX_EPOCH};
  SystemTime::now()
    .duration_since(UNIX_EPOCH)
    .map(|d| d.as_secs())
    .unwrap_or(0)
}
