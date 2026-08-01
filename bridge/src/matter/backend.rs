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
