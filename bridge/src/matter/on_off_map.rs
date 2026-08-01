//! Pure OnOff mapping between HA entity state and Matter controller commands.

use uuid::Uuid;

use crate::catalog::{CommandKind, CommandRequest, DeviceType, Export, HaStateValue};

/// Interpret an HA entity state string as on/off when possible.
pub fn ha_state_is_on(state: &str) -> Option<bool> {
  match state.trim().to_ascii_lowercase().as_str() {
    "on" | "true" | "1" | "open" | "home" | "locked" => Some(true),
    "off" | "false" | "0" | "closed" | "not_home" | "unlocked" => Some(false),
    _ => None,
  }
}

/// Build a Matter→HA OnOff command for an export.
pub fn on_off_command(export_id: Uuid, on: bool) -> CommandRequest {
  CommandRequest {
    export_id,
    kind: CommandKind::OnOff,
    on: Some(on),
    level: None,
    position: None,
  }
}

/// Resolve the on/off value for an export from a batch of HA state values.
///
/// Prefers the primary entity id; falls back to the first parsable on/off state.
pub fn on_off_from_states(export: &Export, states: &[HaStateValue]) -> Option<bool> {
  if !export.type_.is_on_off_capable() {
    return None;
  }
  for st in states {
    if st.entity_id == export.primary_entity_id
      && let Some(on) = ha_state_is_on(&st.state)
    {
      return Some(on);
    }
  }
  for st in states {
    if let Some(on) = ha_state_is_on(&st.state) {
      return Some(on);
    }
  }
  None
}

/// Whether this export should be published as a Matter OnOff endpoint.
pub fn is_matter_on_off_export(export: &Export) -> bool {
  export.enabled && export.type_.is_on_off_capable()
}

/// Pick the primary OnOff export from a catalog (stable: lowest endpoint_id, then name).
pub fn primary_on_off_export(exports: &[Export]) -> Option<&Export> {
  let mut candidates: Vec<&Export> = exports.iter().filter(|e| is_matter_on_off_export(e)).collect();
  candidates.sort_by_key(|e| (e.endpoint_id.unwrap_or(u16::MAX), e.name.as_str()));
  candidates.into_iter().next()
}

/// HA domain service suggestion for an OnOff command (pure; no HA import).
pub fn ha_service_for_on_off(entity_id: &str, on: bool) -> (&'static str, &'static str) {
  let domain = entity_id.split('.').next().unwrap_or("switch");
  let service = if on { "turn_on" } else { "turn_off" };
  match domain {
    "light" => ("light", service),
    "input_boolean" => ("input_boolean", service),
    "cover" => {
      if on {
        ("cover", "open_cover")
      } else {
        ("cover", "close_cover")
      }
    }
    _ => ("switch", service),
  }
}

/// First-ship OnOff-capable Matter device types.
#[allow(dead_code)]
pub fn on_off_device_types() -> [DeviceType; 4] {
  [
    DeviceType::Light,
    DeviceType::OnOffSwitch,
    DeviceType::OnOffPlug,
    DeviceType::Outlet,
  ]
}

#[cfg(test)]
mod tests {
  use super::*;
  use std::collections::BTreeMap;

  fn exp(id: Uuid, entity: &str, type_: DeviceType, enabled: bool, ep: Option<u16>) -> Export {
    Export {
      export_id: id,
      name: "t".into(),
      type_,
      primary_entity_id: entity.into(),
      linked: BTreeMap::new(),
      area_id: None,
      enabled,
      endpoint_id: ep,
    }
  }

  #[test]
  fn parses_common_on_off_states() {
    assert_eq!(ha_state_is_on("on"), Some(true));
    assert_eq!(ha_state_is_on("OFF"), Some(false));
    assert_eq!(ha_state_is_on("unavailable"), None);
  }

  #[test]
  fn prefers_primary_entity_state() {
    let id = Uuid::new_v4();
    let e = exp(id, "light.a", DeviceType::Light, true, Some(1));
    let states = [
      HaStateValue {
        entity_id: "sensor.x".into(),
        state: "off".into(),
        attributes: Default::default(),
      },
      HaStateValue {
        entity_id: "light.a".into(),
        state: "on".into(),
        attributes: Default::default(),
      },
    ];
    assert_eq!(on_off_from_states(&e, &states), Some(true));
  }

  #[test]
  fn primary_export_picks_lowest_endpoint() {
    let a = exp(Uuid::new_v4(), "light.a", DeviceType::Light, true, Some(2));
    let b = exp(Uuid::new_v4(), "switch.b", DeviceType::OnOffSwitch, true, Some(1));
    let list = [a.clone(), b.clone()];
    let p = primary_on_off_export(&list).unwrap();
    assert_eq!(p.export_id, b.export_id);
  }

  #[test]
  fn disabled_exports_skipped() {
    let a = exp(Uuid::new_v4(), "light.a", DeviceType::Light, false, Some(1));
    assert!(primary_on_off_export(&[a]).is_none());
  }

  #[test]
  fn ha_service_mapping() {
    assert_eq!(ha_service_for_on_off("light.k", true), ("light", "turn_on"));
    assert_eq!(ha_service_for_on_off("switch.k", false), ("switch", "turn_off"));
  }

  #[test]
  fn command_shape() {
    let id = Uuid::new_v4();
    let c = on_off_command(id, true);
    assert_eq!(c.kind, CommandKind::OnOff);
    assert_eq!(c.on, Some(true));
    assert_eq!(c.export_id, id);
  }
}
