//! Wire response types for the control plane.

use serde::{Deserialize, Serialize};

use crate::catalog::CommandRequest;

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
  pub export_count: u32,
  pub enabled_export_count: u32,
  pub error: Option<String>,
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
