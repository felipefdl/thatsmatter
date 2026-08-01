//! Shared application state for the IPC server.

use std::sync::Arc;

use parking_lot::Mutex;

use crate::catalog::CatalogStore;
use crate::config::BackendKind;
use crate::matter::MatterBackend;

/// Process-wide state shared by axum handlers.
pub struct AppState {
  pub catalog: Mutex<CatalogStore>,
  pub backend: Arc<dyn MatterBackend>,
  pub backend_kind: BackendKind,
}

impl AppState {
  pub fn new(catalog: CatalogStore, backend: Arc<dyn MatterBackend>) -> Self {
    let backend_kind = backend.kind();
    Self {
      catalog: Mutex::new(catalog),
      backend,
      backend_kind,
    }
  }
}
