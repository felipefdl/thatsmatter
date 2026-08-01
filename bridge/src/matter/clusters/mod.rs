//! Hand-written Matter cluster handlers over rs-matter generated decls.
//!
//! rs-matter 0.2 ships full generated scaffolding for every standard cluster but
//! only a handful of hand-written state machines. Contact, occupancy, and window
//! covering live here as thin `ClusterHandler` adapters over our slot state.

pub mod boolean_state;
pub mod occupancy;
pub mod window_covering;
