//! Matter backend abstraction and implementations.

mod backend;
mod clusters;
pub(crate) mod commissioning;
mod dev;
mod device_types;
mod export_plane;
mod on_off_map;
pub(crate) mod pairing;
mod rs_matter_backend;

pub use backend::MatterBackend;
pub use commissioning::CommissioningMaterial;
pub use dev::DevMatterBackend;
pub use on_off_map::{
  ha_service_for_on_off, ha_state_is_on, is_matter_bridged_export, is_matter_on_off_export, on_off_command,
  on_off_from_states,
};
pub use pairing::pairing_material_for;
pub use rs_matter_backend::RsMatterBackend;
