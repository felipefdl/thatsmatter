//! HTTP handlers for the loopback control plane.

use std::sync::Arc;

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use uuid::Uuid;

use super::types::{BridgeStatus, ErrorBody, HealthResponse, PendingCommands};
use crate::catalog::{CatalogError, CreateExport, Export, HaStateUpdate, HaStateValue, PatchExport, StatePushResult};
use crate::state::AppState;

const VERSION: &str = env!("CARGO_PKG_VERSION");

type SharedState = Arc<AppState>;

/// Convert catalog errors to HTTP responses.
fn catalog_err(err: CatalogError) -> Response {
  match err {
    CatalogError::NotFound(id) => (
      StatusCode::NOT_FOUND,
      Json(ErrorBody {
        error: "not_found".into(),
        message: format!("export not found: {id}"),
      }),
    )
      .into_response(),
    CatalogError::Invalid(msg) => (
      StatusCode::BAD_REQUEST,
      Json(ErrorBody {
        error: "invalid".into(),
        message: msg,
      }),
    )
      .into_response(),
    other => (
      StatusCode::INTERNAL_SERVER_ERROR,
      Json(ErrorBody {
        error: "internal".into(),
        message: other.to_string(),
      }),
    )
      .into_response(),
  }
}

fn internal(msg: impl Into<String>) -> Response {
  (
    StatusCode::INTERNAL_SERVER_ERROR,
    Json(ErrorBody {
      error: "internal".into(),
      message: msg.into(),
    }),
  )
    .into_response()
}

/// `GET /health`
pub async fn health() -> Json<HealthResponse> {
  Json(HealthResponse {
    ok: true,
    version: VERSION.to_string(),
  })
}

/// `GET /status`
pub async fn status(State(state): State<SharedState>) -> Response {
  let (bridge_name, export_count, enabled_export_count) = {
    let cat = state.catalog.lock();
    (
      cat.bridge_name().to_string(),
      cat.export_count() as u32,
      cat.enabled_export_count() as u32,
    )
  };
  let running = state.backend.is_running().await;
  let pairing_open = state.backend.pairing_open().await;
  let error = state.backend.status_error().await;
  Json(BridgeStatus {
    bridge_name,
    running,
    matter_backend: state.backend_kind.as_wire_str().to_string(),
    pairing_open,
    export_count,
    enabled_export_count,
    error,
  })
  .into_response()
}

/// `GET /pairing`
pub async fn pairing(State(state): State<SharedState>) -> Response {
  let material = state.backend.pairing_info().await;
  Json(material).into_response()
}

/// `GET /exports`
pub async fn list_exports(State(state): State<SharedState>) -> Json<Vec<Export>> {
  Json(state.catalog.lock().list())
}

/// `GET /exports/{id}`
pub async fn get_export(State(state): State<SharedState>, Path(id): Path<Uuid>) -> Response {
  match state.catalog.lock().get(id) {
    Some(exp) => Json(exp).into_response(),
    None => catalog_err(CatalogError::NotFound(id)),
  }
}

/// `POST /exports`
pub async fn create_export(State(state): State<SharedState>, Json(body): Json<CreateExport>) -> Response {
  let created = {
    let mut cat = state.catalog.lock();
    match cat.create(body) {
      Ok(exp) => exp,
      Err(e) => return catalog_err(e),
    }
  };
  if let Err(e) = sync_exports(&state).await {
    return internal(e.to_string());
  }
  (StatusCode::CREATED, Json(created)).into_response()
}

/// `PATCH /exports/{id}`
pub async fn patch_export(
  State(state): State<SharedState>,
  Path(id): Path<Uuid>,
  Json(body): Json<PatchExport>,
) -> Response {
  let updated = {
    let mut cat = state.catalog.lock();
    match cat.patch(id, body) {
      Ok(exp) => exp,
      Err(e) => return catalog_err(e),
    }
  };
  if let Err(e) = sync_exports(&state).await {
    return internal(e.to_string());
  }
  Json(updated).into_response()
}

/// `DELETE /exports/{id}`
pub async fn delete_export(State(state): State<SharedState>, Path(id): Path<Uuid>) -> Response {
  let deleted = {
    let mut cat = state.catalog.lock();
    match cat.delete(id) {
      Ok(exp) => exp,
      Err(e) => return catalog_err(e),
    }
  };
  if let Err(e) = sync_exports(&state).await {
    return internal(e.to_string());
  }
  Json(deleted).into_response()
}

/// `POST /exports/{id}/state` — HA pushes state for entities backing this export.
///
/// Accepts either a single `HaStateValue` or a full `HaStateUpdate`.
pub async fn push_state(State(state): State<SharedState>, Path(id): Path<Uuid>, body: axum::body::Bytes) -> Response {
  {
    let cat = state.catalog.lock();
    if cat.get(id).is_none() {
      return catalog_err(CatalogError::NotFound(id));
    }
  }

  let states = match parse_state_body(&body) {
    Ok(s) => s,
    Err(msg) => {
      return (
        StatusCode::BAD_REQUEST,
        Json(ErrorBody {
          error: "invalid".into(),
          message: msg,
        }),
      )
        .into_response();
    }
  };

  match state.backend.apply_state(id, &states).await {
    Ok(applied) => Json(StatePushResult { applied }).into_response(),
    Err(e) => internal(e.to_string()),
  }
}

/// `GET /commands` — poll and drain pending Matter → HA commands.
pub async fn take_commands(State(state): State<SharedState>) -> Json<PendingCommands> {
  let commands = state.backend.take_commands().await;
  Json(PendingCommands { commands })
}

async fn sync_exports(state: &AppState) -> anyhow::Result<()> {
  let exports = state.catalog.lock().list();
  state.backend.set_exports(&exports).await
}

fn parse_state_body(body: &[u8]) -> Result<Vec<HaStateValue>, String> {
  if body.is_empty() {
    return Err("empty body".into());
  }
  // Prefer HaStateUpdate shape.
  if let Ok(update) = serde_json::from_slice::<HaStateUpdate>(body) {
    return Ok(update.states);
  }
  // Fall back to a single HaStateValue.
  if let Ok(one) = serde_json::from_slice::<HaStateValue>(body) {
    return Ok(vec![one]);
  }
  Err("body must be HaStateUpdate or HaStateValue JSON".into())
}
