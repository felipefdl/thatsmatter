//! Wire response types for the control plane.

use serde::{Deserialize, Serialize};

use crate::catalog::CommandRequest;
use crate::matter::PAIRING_TIMEOUT_DEFAULT_SECS;

/// `GET /health` body (protocol `HealthResponse`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HealthResponse {
  pub ok: bool,
  pub version: String,
}

/// `GET /status` body (protocol `BridgeStatus`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BridgeStatus {
  pub bridge_name: String,
  pub running: bool,
  pub matter_backend: String,
  pub pairing_open: bool,
  pub commissioned_fabrics: u8,
  pub export_count: u32,
  pub enabled_export_count: u32,
  pub error: Option<String>,
}

/// Optional body for `POST /pairing/open`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpenPairingRequest {
  #[serde(default = "default_pairing_timeout")]
  pub timeout_secs: u16,
}

fn default_pairing_timeout() -> u16 {
  PAIRING_TIMEOUT_DEFAULT_SECS
}

impl Default for OpenPairingRequest {
  fn default() -> Self {
    Self {
      timeout_secs: PAIRING_TIMEOUT_DEFAULT_SECS,
    }
  }
}

/// `POST /pairing/open` response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpenPairingResponse {
  pub pairing_open: bool,
  pub timeout_secs: u16,
}

/// `POST /pairing/close` response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClosePairingResponse {
  pub pairing_open: bool,
}

/// Error JSON body (protocol `ErrorBody`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErrorBody {
  pub error: String,
  pub message: String,
}

/// `GET /commands` body (protocol `PendingCommands`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PendingCommands {
  pub commands: Vec<CommandRequest>,
}
