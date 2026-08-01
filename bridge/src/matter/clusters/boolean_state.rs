//! Boolean State cluster (0x0045) for contact sensors.
//!
//! Matter semantics: `state_value == true` means closed/normal; `false` means
//! open/alarm. HA binary_sensor contact classes report `on` when open/detected,
//! so the plane stores `closed = !ha_state_is_on(...)`.

use rs_matter::dm::Cluster;
use rs_matter::dm::clusters::decl::boolean_state::{self, FULL_CLUSTER};
use rs_matter::with;

/// Mandatory attributes only; `StateChange` event suppressed (zero-sized event ring).
pub const CLUSTER: Cluster<'static> = FULL_CLUSTER
  .with_attrs(with!(required))
  .with_cmds(with!())
  .with_events(with!());

/// Matter `StateValue` from HA-on semantics (`true` = open/detected).
#[inline]
pub fn state_value_from_ha_on(ha_on: bool) -> bool {
  !ha_on
}

pub use boolean_state::{AttributeId, ClusterHandler, HandlerAdaptor};

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn contact_on_maps_to_state_value_false() {
    assert!(!state_value_from_ha_on(true), "HA on (open) → Matter false");
    assert!(state_value_from_ha_on(false), "HA off (closed) → Matter true");
  }
}
