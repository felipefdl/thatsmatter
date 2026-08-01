//! Integration tests for catalog store endpoint assignment.

use std::collections::BTreeMap;

use thatsmatter_bridge::catalog::{CatalogSnapshot, CatalogStore, DeviceType, Export};
use uuid::Uuid;

#[test]
fn replace_assigns_endpoint_ids_and_preserves_by_export_id() {
  let dir = tempfile::tempdir().unwrap();
  let mut store = CatalogStore::new(dir.path(), "ThatsMatter");
  let id_a = Uuid::parse_str("11111111-1111-4111-8111-111111111111").unwrap();
  let id_b = Uuid::parse_str("22222222-2222-4222-8222-222222222222").unwrap();

  let snap = CatalogSnapshot {
    bridge_name: "Home".into(),
    exports: vec![
      Export {
        export_id: id_a,
        name: "A".into(),
        type_: DeviceType::Light,
        primary_entity_id: "light.a".into(),
        linked: BTreeMap::new(),
        area_id: None,
        enabled: true,
        endpoint_id: None,
      },
      Export {
        export_id: id_b,
        name: "B".into(),
        type_: DeviceType::Outlet,
        primary_entity_id: "switch.b".into(),
        linked: BTreeMap::new(),
        area_id: None,
        enabled: true,
        endpoint_id: None,
      },
    ],
  };

  let out = store.replace(snap).unwrap();
  assert_eq!(out.bridge_name, "Home");
  let ep_a = out.exports.iter().find(|e| e.export_id == id_a).unwrap().endpoint_id;
  let ep_b = out.exports.iter().find(|e| e.export_id == id_b).unwrap().endpoint_id;
  assert!(ep_a.is_some() && ep_b.is_some());
  assert_ne!(ep_a, ep_b);

  let snap2 = CatalogSnapshot {
    bridge_name: "Home".into(),
    exports: vec![Export {
      export_id: id_a,
      name: "A renamed".into(),
      type_: DeviceType::Light,
      primary_entity_id: "light.a2".into(),
      linked: BTreeMap::new(),
      area_id: Some("kitchen".into()),
      enabled: false,
      endpoint_id: None,
    }],
  };
  let out2 = store.replace(snap2).unwrap();
  assert_eq!(out2.exports.len(), 1);
  assert_eq!(out2.exports[0].endpoint_id, ep_a);
  assert_eq!(out2.exports[0].name, "A renamed");
  assert!(!out2.exports[0].enabled);
}
