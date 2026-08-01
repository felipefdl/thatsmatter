//! Axum router and serve loop bound to loopback only.

use std::net::SocketAddr;
use std::sync::Arc;

use axum::Router;
use axum::routing::{get, post};
use tower_http::trace::TraceLayer;

use super::handlers;
use crate::state::AppState;

/// Build the control-plane router.
pub fn router(state: Arc<AppState>) -> Router {
  Router::new()
    .route("/health", get(handlers::health))
    .route("/status", get(handlers::status))
    .route("/pairing", get(handlers::pairing))
    .route("/pairing/open", post(handlers::open_pairing))
    .route("/pairing/close", post(handlers::close_pairing))
    .route("/exports", get(handlers::list_exports).post(handlers::create_export))
    .route(
      "/exports/{id}",
      get(handlers::get_export)
        .patch(handlers::patch_export)
        .delete(handlers::delete_export),
    )
    .route("/exports/{id}/state", post(handlers::push_state))
    .route("/commands", get(handlers::take_commands))
    .layer(TraceLayer::new_for_http())
    .with_state(state)
}

/// Bind `addr` (must already be loopback) and serve until the process is stopped.
pub async fn serve(addr: SocketAddr, state: Arc<AppState>) -> anyhow::Result<()> {
  let app = router(state);
  let listener = tokio::net::TcpListener::bind(addr).await?;
  let local = listener.local_addr()?;
  tracing::info!(%local, "IPC control plane listening");
  axum::serve(listener, app).await?;
  Ok(())
}
