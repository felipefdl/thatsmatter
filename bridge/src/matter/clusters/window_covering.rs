//! Window Covering cluster (0x0102) for cover and garage exports.
//!
//! Features: `LIFT | POSITION_AWARE_LIFT` only (percent-based, no tilt).
//!
//! Position mapping (locked):
//! - Matter percent100ths: `0` = fully open, `10000` = fully closed
//! - HA `current_position`: `100` = open, `0` = closed
//! - `percent100ths = (100 - ha_position) * 100`

use rs_matter::dm::clusters::decl::window_covering::{
  self, CommandId, ConfigStatus, EndProductType, FULL_CLUSTER, Feature, Mode, OperationalStatus, Type,
};

use rs_matter::dm::Cluster;
pub use rs_matter::dm::clusters::decl::window_covering::AttributeId;
use rs_matter::error::ErrorCode;
use rs_matter::with;

use crate::catalog::HaStateValue;

/// Lift + position-aware lift; tilt unsupported.
const FEATURES: u32 = Feature::LIFT.bits() | Feature::POSITION_AWARE_LIFT.bits();

/// Mandatory attrs plus the two position-aware lift percent100ths attributes.
/// Commands: open / close / stop / go-to-lift-percentage. No events.
pub const CLUSTER: Cluster<'static> = FULL_CLUSTER
  .with_features(FEATURES)
  .with_attrs(with!(
    required;
    AttributeId::CurrentPositionLiftPercent100ths | AttributeId::TargetPositionLiftPercent100ths
  ))
  .with_cmds(with!(
    CommandId::UpOrOpen | CommandId::DownOrClose | CommandId::StopMotion | CommandId::GoToLiftPercentage
  ))
  .with_events(with!());

/// HA position (0 = closed, 100 = open) → Matter percent100ths (0 = open, 10000 = closed).
#[inline]
pub fn ha_position_to_percent100ths(ha_position: u8) -> u16 {
  let ha = u16::from(ha_position.min(100));
  (100 - ha) * 100
}

/// Matter percent100ths → HA position (0 = closed, 100 = open).
///
/// Caller must pass a value in `0..=10000` (e.g. after [`validate_lift_percent100ths`]).
/// Values above 10000 are treated as fully closed so a defensive path never panics.
#[inline]
pub fn percent100ths_to_ha_position(percent100ths: u16) -> u8 {
  let p = percent100ths.min(10_000);
  ((10_000 - p) / 100) as u8
}

/// Spec range for `GoToLiftPercentage.lift_percent_100_ths_value` (percent100ths).
///
/// Out of range returns `ConstraintError` so the command path can reject with no
/// state mutation and no queued HA command.
#[inline]
pub fn validate_lift_percent100ths(percent100ths: u16) -> Result<u16, ErrorCode> {
  if percent100ths > 10_000 {
    Err(ErrorCode::ConstraintError)
  } else {
    Ok(percent100ths)
  }
}

/// ConfigStatus for a position-aware lift covering that is online.
#[inline]
pub fn config_status() -> ConfigStatus {
  ConfigStatus::OPERATIONAL | ConfigStatus::LIFT_POSITION_AWARE
}

/// Operational status from HA-scale position/target and the moving flag.
///
/// Matter packs two-bit fields: GLOBAL bits 0–1, LIFT bits 2–3.
/// Values: 0 = Stopped, 1 = Opening, 2 = Closing.
pub fn operational_status(position: u8, target: u8, moving: bool) -> OperationalStatus {
  if !moving || position == target {
    return OperationalStatus::empty();
  }
  // HA scale: higher position = more open. Target > position means opening.
  let direction: u8 = if target > position { 1 } else { 2 };
  OperationalStatus::from_bits_truncate(direction | (direction << 2))
}

/// Product type: roller shade for cover, roller shutter for garage.
#[inline]
pub fn cover_type(is_garage: bool) -> Type {
  if is_garage { Type::Shutter } else { Type::RollerShade }
}

#[inline]
pub fn end_product_type(is_garage: bool) -> EndProductType {
  if is_garage {
    EndProductType::RollerShutter
  } else {
    EndProductType::RollerShade
  }
}

/// Mode is mandatory and writable. Slot shape has no mode state: always empty.
#[inline]
pub fn mode() -> Mode {
  Mode::empty()
}

/// Accept empty Mode writes; reject any non-empty bit set.
///
/// We do not store Mode. Succeeding while discarding bits would lie to the
/// controller, so unsupported bits return `ConstraintError`.
#[inline]
pub fn accept_mode_write(value: Mode) -> Result<(), ErrorCode> {
  if value.is_empty() {
    Ok(())
  } else {
    Err(ErrorCode::ConstraintError)
  }
}

/// Motion inferred from the HA cover entity state string.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CoverMotion {
  Stopped,
  Opening,
  Closing,
}

/// Parsed HA cover state for `apply_state`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CoverHaState {
  /// HA position 0–100 when known.
  pub position: Option<u8>,
  pub motion: CoverMotion,
}

/// Read cover position/motion from an HA state value (primary entity).
///
/// Position prefers `attributes.current_position`; falls back to state
/// `open` → 100 / `closed` → 0. Motion from `opening` / `closing`.
pub fn ha_cover_from_state(value: &HaStateValue) -> CoverHaState {
  let motion = match value.state.trim().to_ascii_lowercase().as_str() {
    "opening" => CoverMotion::Opening,
    "closing" => CoverMotion::Closing,
    _ => CoverMotion::Stopped,
  };

  let position = value
    .attributes
    .get("current_position")
    .and_then(|v| {
      v.as_u64()
        .or_else(|| v.as_f64().map(|f| f as u64))
        .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
    })
    .map(|n| n.min(100) as u8)
    .or_else(|| match value.state.trim().to_ascii_lowercase().as_str() {
      "open" => Some(100),
      "closed" => Some(0),
      _ => None,
    });

  CoverHaState { position, motion }
}

pub use window_covering::{ClusterHandler, HandlerAdaptor};

#[cfg(test)]
mod tests {
  use super::*;
  use serde_json::json;

  #[test]
  fn percent100ths_round_trip_endpoints() {
    assert_eq!(ha_position_to_percent100ths(100), 0);
    assert_eq!(ha_position_to_percent100ths(0), 10_000);
    assert_eq!(percent100ths_to_ha_position(0), 100);
    assert_eq!(percent100ths_to_ha_position(10_000), 0);
  }

  #[test]
  fn percent100ths_round_trip_midpoints() {
    assert_eq!(ha_position_to_percent100ths(37), 6300);
    assert_eq!(percent100ths_to_ha_position(6300), 37);
    assert_eq!(ha_position_to_percent100ths(50), 5000);
    assert_eq!(percent100ths_to_ha_position(5000), 50);
    for ha in [0u8, 1, 25, 37, 50, 75, 99, 100] {
      let p = ha_position_to_percent100ths(ha);
      assert_eq!(percent100ths_to_ha_position(p), ha, "round-trip ha={ha}");
    }
  }

  #[test]
  fn ha_position_clamps_above_100() {
    assert_eq!(ha_position_to_percent100ths(200), 0);
  }

  #[test]
  fn lift_percent100ths_range_is_0_to_10000() {
    assert_eq!(validate_lift_percent100ths(0), Ok(0));
    assert_eq!(validate_lift_percent100ths(6300), Ok(6300));
    assert_eq!(validate_lift_percent100ths(10_000), Ok(10_000));
    assert_eq!(validate_lift_percent100ths(10_001), Err(ErrorCode::ConstraintError));
  }

  #[test]
  fn mode_write_accepts_empty_rejects_bits() {
    assert_eq!(accept_mode_write(Mode::empty()), Ok(()));
    assert_eq!(
      accept_mode_write(Mode::MOTOR_DIRECTION_REVERSED),
      Err(ErrorCode::ConstraintError)
    );
    assert_eq!(
      accept_mode_write(Mode::CALIBRATION_MODE | Mode::LED_FEEDBACK),
      Err(ErrorCode::ConstraintError)
    );
  }

  #[test]
  fn operational_status_from_motion() {
    assert_eq!(operational_status(50, 50, false), OperationalStatus::empty());
    assert_eq!(operational_status(50, 100, false), OperationalStatus::empty());
    // Opening: GLOBAL=1, LIFT=1 → bits 0b0101
    assert_eq!(
      operational_status(20, 100, true),
      OperationalStatus::from_bits_truncate(0b0101)
    );
    // Closing: GLOBAL=2, LIFT=2 → bits 0b1010
    assert_eq!(
      operational_status(80, 0, true),
      OperationalStatus::from_bits_truncate(0b1010)
    );
  }

  #[test]
  fn ha_cover_reads_current_position_attribute() {
    let mut attrs = serde_json::Map::new();
    attrs.insert("current_position".into(), json!(37));
    let st = HaStateValue {
      entity_id: "cover.shade".into(),
      state: "open".into(),
      attributes: attrs,
    };
    let parsed = ha_cover_from_state(&st);
    assert_eq!(parsed.position, Some(37));
    assert_eq!(parsed.motion, CoverMotion::Stopped);
  }

  #[test]
  fn ha_cover_falls_back_to_open_closed_state() {
    let open = HaStateValue {
      entity_id: "cover.a".into(),
      state: "open".into(),
      attributes: Default::default(),
    };
    assert_eq!(ha_cover_from_state(&open).position, Some(100));

    let closed = HaStateValue {
      entity_id: "cover.a".into(),
      state: "closed".into(),
      attributes: Default::default(),
    };
    assert_eq!(ha_cover_from_state(&closed).position, Some(0));
  }

  #[test]
  fn ha_cover_detects_opening_closing() {
    let opening = HaStateValue {
      entity_id: "cover.a".into(),
      state: "opening".into(),
      attributes: Default::default(),
    };
    assert_eq!(ha_cover_from_state(&opening).motion, CoverMotion::Opening);

    let closing = HaStateValue {
      entity_id: "cover.a".into(),
      state: "closing".into(),
      attributes: Default::default(),
    };
    assert_eq!(ha_cover_from_state(&closing).motion, CoverMotion::Closing);
  }
}
