//! Matter Device Library device types that `rs-matter` 0.2 does not declare.
//!
//! `rs_matter::dm::devices` ships only the handful of device types its own
//! examples need. `DeviceType` has public fields, so the rest are declared here
//! in one reviewable place. Revisions are the Matter Device Library 1.4 values.

use rs_matter::dm::DeviceType;

/// On/Off Plug-in Unit (0x010A): a mains outlet or relay exposed as on/off.
pub const DEV_TYPE_ON_OFF_PLUG_IN_UNIT: DeviceType = DeviceType { dtype: 0x010A, drev: 3 };

/// Contact Sensor (0x0015): open/closed reported through Boolean State.
#[allow(dead_code)]
pub const DEV_TYPE_CONTACT_SENSOR: DeviceType = DeviceType { dtype: 0x0015, drev: 2 };

/// Occupancy Sensor (0x0107): motion/presence reported through Occupancy Sensing.
#[allow(dead_code)]
pub const DEV_TYPE_OCCUPANCY_SENSOR: DeviceType = DeviceType { dtype: 0x0107, drev: 4 };

/// Window Covering (0x0202): blinds/shades driven through Window Covering.
#[allow(dead_code)]
pub const DEV_TYPE_WINDOW_COVERING: DeviceType = DeviceType { dtype: 0x0202, drev: 5 };
