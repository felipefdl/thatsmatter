//! HTTP-level export CRUD, state push, and command queue tests.

use std::collections::BTreeMap;
use std::sync::Arc;

use http_body_util::BodyExt;
use thatsmatter_bridge::AppState;
use thatsmatter_bridge::catalog::{
  CatalogStore, CommandKind, CommandRequest, CreateExport, DeviceType, Export, HaStateValue,
};
use thatsmatter_bridge::ipc;
use thatsmatter_bridge::matter::{DevMatterBackend, MatterBackend};
use tower::ServiceExt;
use uuid::Uuid;

async fn test_app() -> (axum::Router, Arc<DevMatterBackend>, tempfile::TempDir) {
  let dir = tempfile::tempdir().unwrap();
  let catalog = CatalogStore::new(dir.path(), "ThatsMatter");
  let backend = Arc::new(DevMatterBackend::new(dir.path()));
  backend.start().await.unwrap();
  let state = Arc::new(AppState::new(catalog, backend.clone() as Arc<dyn MatterBackend>));
  (ipc::router(state), backend, dir)
}

async fn body_json(res: axum::response::Response) -> serde_json::Value {
  let bytes = res.into_body().collect().await.unwrap().to_bytes();
  serde_json::from_slice(&bytes).unwrap()
}

#[tokio::test]
async fn health_and_status() {
  let (app, _, _dir) = test_app().await;

  let res = app
    .clone()
    .oneshot(
      axum::http::Request::builder()
        .uri("/health")
        .body(axum::body::Body::empty())
        .unwrap(),
    )
    .await
    .unwrap();
  assert_eq!(res.status(), 200);
  let health = body_json(res).await;
  assert_eq!(health["ok"], true);
  assert!(!health["version"].as_str().unwrap().is_empty());

  let res = app
    .oneshot(
      axum::http::Request::builder()
        .uri("/status")
        .body(axum::body::Body::empty())
        .unwrap(),
    )
    .await
    .unwrap();
  assert_eq!(res.status(), 200);
  let status = body_json(res).await;
  assert_eq!(status["bridge_name"], "ThatsMatter");
  assert_eq!(status["running"], true);
  assert_eq!(status["matter_backend"], "dev");
  assert_eq!(status["export_count"], 0);
  assert_eq!(status["pairing_open"], true);
}

#[tokio::test]
async fn export_crud_and_endpoint_stable() {
  let (app, _, _dir) = test_app().await;

  let create = CreateExport {
    export_id: None,
    name: "Kitchen Lamp".into(),
    type_: DeviceType::Light,
    primary_entity_id: "light.kitchen".into(),
    linked: BTreeMap::new(),
    area_id: Some("kitchen".into()),
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
  assert_eq!(res.status(), 201);
  let created: Export = serde_json::from_value(body_json(res).await).unwrap();
  assert_eq!(created.name, "Kitchen Lamp");
  assert!(created.endpoint_id.is_some());
  let export_id = created.export_id;
  let endpoint_id = created.endpoint_id;

  let res = app
    .clone()
    .oneshot(
      axum::http::Request::builder()
        .uri(format!("/exports/{export_id}"))
        .body(axum::body::Body::empty())
        .unwrap(),
    )
    .await
    .unwrap();
  assert_eq!(res.status(), 200);

  let patch = serde_json::json!({ "name": "Lamp", "enabled": false });
  let res = app
    .clone()
    .oneshot(
      axum::http::Request::builder()
        .method("PATCH")
        .uri(format!("/exports/{export_id}"))
        .header("content-type", "application/json")
        .body(axum::body::Body::from(patch.to_string()))
        .unwrap(),
    )
    .await
    .unwrap();
  assert_eq!(res.status(), 200);
  let patched: Export = serde_json::from_value(body_json(res).await).unwrap();
  assert_eq!(patched.name, "Lamp");
  assert!(!patched.enabled);
  assert_eq!(patched.endpoint_id, endpoint_id);
  // Patch without area_id leaves the area untouched.
  assert_eq!(patched.area_id.as_deref(), Some("kitchen"));

  // Explicit null clears area_id (protocol contract).
  let clear = serde_json::json!({ "area_id": null });
  let res = app
    .clone()
    .oneshot(
      axum::http::Request::builder()
        .method("PATCH")
        .uri(format!("/exports/{export_id}"))
        .header("content-type", "application/json")
        .body(axum::body::Body::from(clear.to_string()))
        .unwrap(),
    )
    .await
    .unwrap();
  assert_eq!(res.status(), 200);
  let cleared: Export = serde_json::from_value(body_json(res).await).unwrap();
  assert_eq!(cleared.area_id, None);

  let res = app
    .clone()
    .oneshot(
      axum::http::Request::builder()
        .uri("/exports")
        .body(axum::body::Body::empty())
        .unwrap(),
    )
    .await
    .unwrap();
  let list: Vec<Export> = serde_json::from_value(body_json(res).await).unwrap();
  assert_eq!(list.len(), 1);

  let res = app
    .oneshot(
      axum::http::Request::builder()
        .method("DELETE")
        .uri(format!("/exports/{export_id}"))
        .body(axum::body::Body::empty())
        .unwrap(),
    )
    .await
    .unwrap();
  assert_eq!(res.status(), 200);
}

#[tokio::test]
async fn state_push_and_command_queue() {
  let (app, backend, _dir) = test_app().await;

  let create = CreateExport {
    export_id: None,
    name: "Plug".into(),
    type_: DeviceType::Outlet,
    primary_entity_id: "switch.plug".into(),
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
  let created: Export = serde_json::from_value(body_json(res).await).unwrap();

  let state_body = HaStateValue {
    entity_id: "switch.plug".into(),
    state: "on".into(),
    attributes: Default::default(),
  };
  let res = app
    .clone()
    .oneshot(
      axum::http::Request::builder()
        .method("POST")
        .uri(format!("/exports/{}/state", created.export_id))
        .header("content-type", "application/json")
        .body(axum::body::Body::from(serde_json::to_vec(&state_body).unwrap()))
        .unwrap(),
    )
    .await
    .unwrap();
  assert_eq!(res.status(), 200);
  let applied = body_json(res).await;
  assert_eq!(applied["applied"], 1);
  assert_eq!(backend.entity_state("switch.plug").unwrap().state, "on");

  backend.push_command(CommandRequest {
    export_id: created.export_id,
    kind: CommandKind::OnOff,
    on: Some(false),
    level: None,
    position: None,
  });

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
  let pending = body_json(res).await;
  assert_eq!(pending["commands"].as_array().unwrap().len(), 1);
  assert_eq!(pending["commands"][0]["kind"], "on_off");
  assert_eq!(pending["commands"][0]["on"], false);

  // Queue drained.
  let res = app
    .oneshot(
      axum::http::Request::builder()
        .uri("/commands")
        .body(axum::body::Body::empty())
        .unwrap(),
    )
    .await
    .unwrap();
  let pending = body_json(res).await;
  assert_eq!(pending["commands"].as_array().unwrap().len(), 0);
}

#[tokio::test]
async fn pairing_returns_placeholders() {
  let (app, _, _dir) = test_app().await;
  let res = app
    .oneshot(
      axum::http::Request::builder()
        .uri("/pairing")
        .body(axum::body::Body::empty())
        .unwrap(),
    )
    .await
    .unwrap();
  assert_eq!(res.status(), 200);
  let p = body_json(res).await;
  assert!(!p["setup_code"].as_str().unwrap().is_empty());
  assert!(!p["qr_payload"].as_str().unwrap().is_empty());
  assert!(p["discriminator"].as_u64().is_some());
  assert!(p["passcode"].as_u64().is_some());
}

#[tokio::test]
async fn missing_export_is_404() {
  let (app, _, _dir) = test_app().await;
  let id = Uuid::nil();
  let res = app
    .oneshot(
      axum::http::Request::builder()
        .uri(format!("/exports/{id}"))
        .body(axum::body::Body::empty())
        .unwrap(),
    )
    .await
    .unwrap();
  assert_eq!(res.status(), 404);
}
