//! OnOff state push and controller command path through the real IPC handlers.

use std::collections::BTreeMap;
use std::sync::Arc;

use http_body_util::BodyExt;
use thatsmatter_bridge::AppState;
use thatsmatter_bridge::catalog::{CatalogStore, CreateExport, DeviceType};
use thatsmatter_bridge::ipc;
use thatsmatter_bridge::matter::{DevMatterBackend, MatterBackend};
use tower::ServiceExt;
use uuid::Uuid;

async fn body_json(res: axum::response::Response) -> serde_json::Value {
  let bytes = res.into_body().collect().await.unwrap().to_bytes();
  serde_json::from_slice(&bytes).unwrap()
}

#[tokio::test]
async fn ha_state_push_and_controller_command_roundtrip() {
  let dir = tempfile::tempdir().unwrap();
  let catalog = CatalogStore::new(dir.path(), "ThatsMatter");
  let backend = Arc::new(DevMatterBackend::new(dir.path()).unwrap());
  backend.start().await.unwrap();
  let state = Arc::new(AppState::new(catalog, backend.clone() as Arc<dyn MatterBackend>));
  let app = ipc::router(state);

  let export_id = Uuid::parse_str("aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee").unwrap();
  let create = CreateExport {
    export_id: Some(export_id),
    name: "Desk Lamp".into(),
    type_: DeviceType::Light,
    primary_entity_id: "light.desk".into(),
    linked: BTreeMap::new(),
    area_id: None,
    enabled: true,
  };

  let res = app
    .clone()
    .oneshot(
      axum::http::Request::builder()
        .method("POST")
        .uri("/exports")
        .header("content-type", "application/json")
        .body(axum::body::Body::from(serde_json::to_vec(&create).unwrap()))
        .unwrap(),
    )
    .await
    .unwrap();
  assert!(res.status().is_success(), "create status {}", res.status());
  let created = body_json(res).await;
  assert_eq!(created["export_id"], export_id.to_string());
  assert!(created["endpoint_id"].as_u64().unwrap() >= 1);

  // HA → bridge state
  let state_body = serde_json::json!({
    "entity_id": "light.desk",
    "state": "on",
    "attributes": {"brightness": 128}
  });
  let res = app
    .clone()
    .oneshot(
      axum::http::Request::builder()
        .method("POST")
        .uri(format!("/exports/{export_id}/state"))
        .header("content-type", "application/json")
        .body(axum::body::Body::from(state_body.to_string()))
        .unwrap(),
    )
    .await
    .unwrap();
  assert_eq!(res.status(), 200);
  let applied = body_json(res).await;
  assert_eq!(applied["applied"], 1);

  // Matter controller → bridge command queue → HA would poll /commands
  backend.simulate_controller_on_off(export_id, false);
  let res = app
    .clone()
    .oneshot(
      axum::http::Request::builder()
        .uri("/commands")
        .body(axum::body::Body::empty())
        .unwrap(),
    )
    .await
    .unwrap();
  assert_eq!(res.status(), 200);
  let cmds = body_json(res).await;
  assert_eq!(cmds["commands"].as_array().unwrap().len(), 1);
  assert_eq!(cmds["commands"][0]["export_id"], export_id.to_string());
  assert_eq!(cmds["commands"][0]["kind"], "on_off");
  assert_eq!(cmds["commands"][0]["on"], false);

  // Drain empty
  let res = app
    .oneshot(
      axum::http::Request::builder()
        .uri("/commands")
        .body(axum::body::Body::empty())
        .unwrap(),
    )
    .await
    .unwrap();
  let cmds = body_json(res).await;
  assert_eq!(cmds["commands"].as_array().unwrap().len(), 0);
}
