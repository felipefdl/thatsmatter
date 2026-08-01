//! Serde types aligned with `protocol/schema.json`.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// First-ship Matter device type keys (protocol `DeviceType` enum).
///
/// User-facing shorthand "on_off" maps to `on_off_switch` / `on_off_plug` / `outlet`
/// as distinct protocol values.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeviceType {
  Light,
  OnOffSwitch,
  OnOffPlug,
  Outlet,
  Contact,
  Motion,
  Cover,
  Garage,
}

impl DeviceType {
  /// Whether this type primarily exposes on/off control.
  pub fn is_on_off_capable(self) -> bool {
    matches!(self, Self::Light | Self::OnOffSwitch | Self::OnOffPlug | Self::Outlet)
  }
}

/// Optional linked entity role keys.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LinkedRole {
  Battery,
  Brightness,
  Position,
}

/// One curated export (device as controllers see it).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Export {
  /// Stable export identity. Never the HA `entity_id`.
  pub export_id: Uuid,
  /// Matter advertised name.
  pub name: String,
  /// Matter device type key.
  #[serde(rename = "type")]
  pub type_: DeviceType,
  /// Primary HA entity backing this export.
  pub primary_entity_id: String,
  /// Optional role → entity_id map.
  #[serde(default)]
  pub linked: BTreeMap<String, String>,
  /// Optional HA area override.
  #[serde(default)]
  pub area_id: Option<String>,
  /// Soft enable/disable without deleting config.
  pub enabled: bool,
  /// Matter endpoint id assigned by the bridge; null until assigned.
  #[serde(default)]
  pub endpoint_id: Option<u16>,
}

/// Full catalog snapshot (protocol `CatalogSnapshot`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CatalogSnapshot {
  pub bridge_name: String,
  pub exports: Vec<Export>,
}

/// Pairing material shown in HA (protocol `PairingMaterial`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PairingMaterial {
  /// Manual Matter setup / pairing code.
  pub setup_code: String,
  /// Matter QR payload string for rendering in HA.
  pub qr_payload: String,
  pub discriminator: u32,
  pub passcode: u32,
}

/// Single HA entity state value (protocol `HaStateValue`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HaStateValue {
  pub entity_id: String,
  pub state: String,
  #[serde(default)]
  pub attributes: serde_json::Map<String, serde_json::Value>,
}

/// HA pushes entity state to the bridge (protocol `HaStateUpdate`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HaStateUpdate {
  pub states: Vec<HaStateValue>,
}

/// Result of applying HA state (protocol `StatePushResult`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StatePushResult {
  pub applied: u32,
}

/// Command kinds from a Matter controller (protocol `CommandKind`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CommandKind {
  OnOff,
  Level,
  CoverPosition,
  CoverOpen,
  CoverClose,
  CoverStop,
}

/// Pending command for HA execution (protocol `CommandRequest`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CommandRequest {
  pub export_id: Uuid,
  pub kind: CommandKind,
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub on: Option<bool>,
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub level: Option<u8>,
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub position: Option<u8>,
}
