//! Occupancy Sensing cluster (0x0406) for motion/presence sensors.
//!
//! Reports a PIR sensor: `occupancy` bit 0 from the slot, sensor type fixed to PIR.

use rs_matter::dm::Cluster;
use rs_matter::dm::clusters::decl::occupancy_sensing::{
  self, FULL_CLUSTER, OccupancyBitmap, OccupancySensorTypeBitmap, OccupancySensorTypeEnum,
};
use rs_matter::with;

/// Mandatory occupancy attributes only; hold-time / delay attrs stay optional.
pub const CLUSTER: Cluster<'static> = FULL_CLUSTER
  .with_attrs(with!(required))
  .with_cmds(with!())
  .with_events(with!());

/// Occupancy bitmap from the slot's occupied flag (bit 0 = Occupied).
#[inline]
pub fn occupancy_bitmap(occupied: bool) -> OccupancyBitmap {
  if occupied {
    OccupancyBitmap::OCCUPIED
  } else {
    OccupancyBitmap::empty()
  }
}

/// Fixed PIR sensor type.
#[inline]
pub fn sensor_type() -> OccupancySensorTypeEnum {
  OccupancySensorTypeEnum::PIR
}

/// Fixed PIR sensor type bitmap.
#[inline]
pub fn sensor_type_bitmap() -> OccupancySensorTypeBitmap {
  OccupancySensorTypeBitmap::PIR
}

pub use occupancy_sensing::{AttributeId, ClusterHandler, HandlerAdaptor};

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn motion_on_sets_occupied_bit() {
    assert!(occupancy_bitmap(true).contains(OccupancyBitmap::OCCUPIED));
    assert!(!occupancy_bitmap(false).contains(OccupancyBitmap::OCCUPIED));
    assert_eq!(sensor_type(), OccupancySensorTypeEnum::PIR);
    assert!(sensor_type_bitmap().contains(OccupancySensorTypeBitmap::PIR));
  }
}
