//! Matter backend abstraction and implementations.

mod backend;
mod dev;
mod on_off_map;
pub(crate) mod pairing;
mod rs_matter_backend;

pub use backend::MatterBackend;
pub use dev::DevMatterBackend;
pub use on_off_map::{
  ha_service_for_on_off, ha_state_is_on, is_matter_on_off_export, on_off_command, on_off_from_states,
  primary_on_off_export,
};
pub use pairing::test_device_pairing_material;
pub use rs_matter_backend::RsMatterBackend;
