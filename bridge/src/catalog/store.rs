//! In-memory catalog with JSON file persistence under `--data-dir`.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

use super::model::{CatalogSnapshot, DeviceType, Export};

const EXPORTS_FILE: &str = "exports.json";
const CONFIG_FILE: &str = "bridge_config.json";
/// Matter root endpoint is 0; dynamic endpoints start at 1.
const FIRST_ENDPOINT_ID: u16 = 1;

/// Errors from catalog mutations and persistence.
#[derive(Debug, Error)]
pub enum CatalogError {
  #[error("export not found: {0}")]
  NotFound(Uuid),
  #[error("invalid export: {0}")]
  Invalid(String),
  #[error("endpoint id space exhausted")]
  EndpointExhausted,
  #[error("io: {0}")]
  Io(#[from] std::io::Error),
  #[error("json: {0}")]
  Json(#[from] serde_json::Error),
}

/// Body for `POST /exports` (export_id optional; bridge assigns if missing).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateExport {
  #[serde(default)]
  pub export_id: Option<Uuid>,
  pub name: String,
  #[serde(rename = "type")]
  pub type_: DeviceType,
  pub primary_entity_id: String,
  #[serde(default)]
  pub linked: BTreeMap<String, String>,
  #[serde(default)]
  pub area_id: Option<String>,
  #[serde(default = "default_enabled")]
  pub enabled: bool,
}

fn default_enabled() -> bool {
  true
}

/// Body for `PATCH /exports/{id}` (all fields optional).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PatchExport {
  #[serde(default)]
  pub name: Option<String>,
  #[serde(default, rename = "type")]
  pub type_: Option<DeviceType>,
  #[serde(default)]
  pub primary_entity_id: Option<String>,
  #[serde(default)]
  pub linked: Option<BTreeMap<String, String>>,
  #[serde(default)]
  pub area_id: Option<Option<String>>,
  #[serde(default)]
  pub enabled: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PersistedConfig {
  bridge_name: String,
  next_endpoint: u16,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PersistedExports {
  exports: Vec<Export>,
}

/// In-memory export catalog with stable endpoint assignment and disk backup.
#[derive(Debug)]
pub struct CatalogStore {
  data_dir: PathBuf,
  bridge_name: String,
  by_id: BTreeMap<Uuid, Export>,
  next_endpoint: u16,
}

impl CatalogStore {
  /// Create an empty store (does not touch disk until save).
  pub fn new(data_dir: impl Into<PathBuf>, bridge_name: impl Into<String>) -> Self {
    Self {
      data_dir: data_dir.into(),
      bridge_name: bridge_name.into(),
      by_id: BTreeMap::new(),
      next_endpoint: FIRST_ENDPOINT_ID,
    }
  }

  /// Load from `data_dir` if files exist; otherwise start empty with the given default name.
  pub fn load_or_new(
    data_dir: impl Into<PathBuf>,
    default_bridge_name: impl Into<String>,
  ) -> Result<Self, CatalogError> {
    let data_dir = data_dir.into();
    let default_bridge_name = default_bridge_name.into();
    std::fs::create_dir_all(&data_dir)?;

    let mut store = Self::new(&data_dir, default_bridge_name.clone());

    let cfg_path = data_dir.join(CONFIG_FILE);
    if cfg_path.exists() {
      let raw = std::fs::read_to_string(&cfg_path)?;
      let cfg: PersistedConfig = serde_json::from_str(&raw)?;
      store.bridge_name = cfg.bridge_name;
      store.next_endpoint = cfg.next_endpoint.max(FIRST_ENDPOINT_ID);
    }

    let exports_path = data_dir.join(EXPORTS_FILE);
    if exports_path.exists() {
      let raw = std::fs::read_to_string(&exports_path)?;
      let file: PersistedExports = serde_json::from_str(&raw)?;
      // Insert persisted endpoint ids first so later allocation cannot collide with them.
      let mut missing: Vec<Uuid> = Vec::new();
      for exp in file.exports {
        if let Some(ep) = exp.endpoint_id {
          store.next_endpoint = store.next_endpoint.max(ep.saturating_add(1));
        } else {
          missing.push(exp.export_id);
        }
        store.by_id.insert(exp.export_id, exp);
      }
      for id in missing {
        let ep = store.alloc_endpoint()?;
        if let Some(exp) = store.by_id.get_mut(&id) {
          exp.endpoint_id = Some(ep);
        }
      }
    }

    Ok(store)
  }

  pub fn data_dir(&self) -> &Path {
    &self.data_dir
  }

  pub fn bridge_name(&self) -> &str {
    &self.bridge_name
  }

  pub fn set_bridge_name(&mut self, name: impl Into<String>) -> Result<(), CatalogError> {
    let name = name.into();
    if name.is_empty() || name.len() > 32 {
      return Err(CatalogError::Invalid("bridge_name must be 1..=32 characters".into()));
    }
    self.bridge_name = name;
    self.persist()?;
    Ok(())
  }

  pub fn snapshot(&self) -> CatalogSnapshot {
    CatalogSnapshot {
      bridge_name: self.bridge_name.clone(),
      exports: self.by_id.values().cloned().collect(),
    }
  }

  pub fn list(&self) -> Vec<Export> {
    self.by_id.values().cloned().collect()
  }

  pub fn get(&self, id: Uuid) -> Option<Export> {
    self.by_id.get(&id).cloned()
  }

  pub fn export_count(&self) -> usize {
    self.by_id.len()
  }

  pub fn enabled_export_count(&self) -> usize {
    self.by_id.values().filter(|e| e.enabled).count()
  }

  /// Replace entire catalog (used by bulk sync / tests). Preserves endpoint_id by export_id.
  pub fn replace(&mut self, incoming: CatalogSnapshot) -> Result<CatalogSnapshot, CatalogError> {
    if incoming.bridge_name.is_empty() || incoming.bridge_name.len() > 32 {
      return Err(CatalogError::Invalid("bridge_name must be 1..=32 characters".into()));
    }
    self.bridge_name = incoming.bridge_name;

    let mut next: BTreeMap<Uuid, Export> = BTreeMap::new();
    for mut exp in incoming.exports {
      Self::validate_export_fields(&exp.name, &exp.primary_entity_id)?;
      if let Some(prev) = self.by_id.get(&exp.export_id) {
        exp.endpoint_id = prev.endpoint_id.or(exp.endpoint_id);
      }
      if exp.endpoint_id.is_none() {
        exp.endpoint_id = Some(self.alloc_endpoint()?);
      }
      next.insert(exp.export_id, exp);
    }
    self.by_id = next;
    self.persist()?;
    Ok(self.snapshot())
  }

  pub fn create(&mut self, body: CreateExport) -> Result<Export, CatalogError> {
    Self::validate_export_fields(&body.name, &body.primary_entity_id)?;
    let export_id = body.export_id.unwrap_or_else(Uuid::new_v4);
    if self.by_id.contains_key(&export_id) {
      return Err(CatalogError::Invalid(format!("export_id already exists: {export_id}")));
    }
    let exp = Export {
      export_id,
      name: body.name,
      type_: body.type_,
      primary_entity_id: body.primary_entity_id,
      linked: body.linked,
      area_id: body.area_id,
      enabled: body.enabled,
      endpoint_id: Some(self.alloc_endpoint()?),
    };
    self.by_id.insert(export_id, exp.clone());
    self.persist()?;
    Ok(exp)
  }

  pub fn patch(&mut self, id: Uuid, body: PatchExport) -> Result<Export, CatalogError> {
    if !self.by_id.contains_key(&id) {
      return Err(CatalogError::NotFound(id));
    }
    // Allocate outside the map borrow if needed.
    let needs_endpoint = self.by_id.get(&id).is_some_and(|e| e.endpoint_id.is_none());
    let new_endpoint = if needs_endpoint {
      Some(self.alloc_endpoint()?)
    } else {
      None
    };

    let exp = self.by_id.get_mut(&id).ok_or(CatalogError::NotFound(id))?;
    if let Some(name) = body.name {
      if name.is_empty() || name.len() > 64 {
        return Err(CatalogError::Invalid("name must be 1..=64 characters".into()));
      }
      exp.name = name;
    }
    if let Some(type_) = body.type_ {
      exp.type_ = type_;
    }
    if let Some(primary) = body.primary_entity_id {
      if primary.is_empty() {
        return Err(CatalogError::Invalid("primary_entity_id must be non-empty".into()));
      }
      exp.primary_entity_id = primary;
    }
    if let Some(linked) = body.linked {
      exp.linked = linked;
    }
    if let Some(area_id) = body.area_id {
      exp.area_id = area_id;
    }
    if let Some(enabled) = body.enabled {
      exp.enabled = enabled;
    }
    // endpoint_id is stable once assigned; only fill if still missing.
    if exp.endpoint_id.is_none() {
      exp.endpoint_id = new_endpoint;
    }
    let out = exp.clone();
    self.persist()?;
    Ok(out)
  }

  pub fn delete(&mut self, id: Uuid) -> Result<Export, CatalogError> {
    let exp = self.by_id.remove(&id).ok_or(CatalogError::NotFound(id))?;
    self.persist()?;
    Ok(exp)
  }

  fn validate_export_fields(name: &str, primary_entity_id: &str) -> Result<(), CatalogError> {
    if name.is_empty() || name.len() > 64 {
      return Err(CatalogError::Invalid("name must be 1..=64 characters".into()));
    }
    if primary_entity_id.is_empty() {
      return Err(CatalogError::Invalid("primary_entity_id must be non-empty".into()));
    }
    Ok(())
  }

  fn alloc_endpoint(&mut self) -> Result<u16, CatalogError> {
    let used: BTreeSet<u16> = self.by_id.values().filter_map(|e| e.endpoint_id).collect();
    // Ids 1..=u16::MAX are assignable (0 is the Matter root endpoint).
    if used.len() >= usize::from(u16::MAX) {
      return Err(CatalogError::EndpointExhausted);
    }
    // Prefer monotonic next_endpoint; wrap past u16::MAX and skip taken ids.
    loop {
      if self.next_endpoint == 0 {
        self.next_endpoint = FIRST_ENDPOINT_ID;
      }
      let candidate = self.next_endpoint;
      self.next_endpoint = self.next_endpoint.wrapping_add(1);
      if !used.contains(&candidate) {
        return Ok(candidate);
      }
    }
  }

  fn persist(&self) -> Result<(), CatalogError> {
    std::fs::create_dir_all(&self.data_dir)?;
    let cfg = PersistedConfig {
      bridge_name: self.bridge_name.clone(),
      next_endpoint: self.next_endpoint,
    };
    let cfg_path = self.data_dir.join(CONFIG_FILE);
    let tmp_cfg = self.data_dir.join(format!("{CONFIG_FILE}.tmp"));
    std::fs::write(&tmp_cfg, serde_json::to_string_pretty(&cfg)?)?;
    std::fs::rename(&tmp_cfg, &cfg_path)?;

    let file = PersistedExports {
      exports: self.by_id.values().cloned().collect(),
    };
    let exp_path = self.data_dir.join(EXPORTS_FILE);
    let tmp_exp = self.data_dir.join(format!("{EXPORTS_FILE}.tmp"));
    std::fs::write(&tmp_exp, serde_json::to_string_pretty(&file)?)?;
    std::fs::rename(&tmp_exp, &exp_path)?;
    Ok(())
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use tempfile::tempdir;

  fn sample_create(name: &str, entity: &str) -> CreateExport {
    CreateExport {
      export_id: None,
      name: name.into(),
      type_: DeviceType::Light,
      primary_entity_id: entity.into(),
      linked: BTreeMap::new(),
      area_id: None,
      enabled: true,
    }
  }

  #[test]
  fn create_assigns_stable_endpoint_and_persists() {
    let dir = tempdir().unwrap();
    let mut store = CatalogStore::new(dir.path(), "ThatsMatter");
    let a = store.create(sample_create("A", "light.a")).unwrap();
    let b = store.create(sample_create("B", "light.b")).unwrap();
    assert!(a.endpoint_id.is_some());
    assert!(b.endpoint_id.is_some());
    assert_ne!(a.endpoint_id, b.endpoint_id);

    let reloaded = CatalogStore::load_or_new(dir.path(), "Other").unwrap();
    assert_eq!(reloaded.bridge_name(), "ThatsMatter");
    assert_eq!(reloaded.export_count(), 2);
    assert_eq!(reloaded.get(a.export_id).unwrap().endpoint_id, a.endpoint_id);
    assert_eq!(reloaded.get(b.export_id).unwrap().endpoint_id, b.endpoint_id);
  }

  #[test]
  fn replace_preserves_endpoint_by_export_id() {
    let dir = tempdir().unwrap();
    let mut store = CatalogStore::new(dir.path(), "ThatsMatter");
    let id_a = Uuid::parse_str("11111111-1111-4111-8111-111111111111").unwrap();
    let id_b = Uuid::parse_str("22222222-2222-4222-8222-222222222222").unwrap();

    let out = store
      .replace(CatalogSnapshot {
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
      })
      .unwrap();

    let ep_a = out.exports.iter().find(|e| e.export_id == id_a).unwrap().endpoint_id;
    let ep_b = out.exports.iter().find(|e| e.export_id == id_b).unwrap().endpoint_id;
    assert!(ep_a.is_some() && ep_b.is_some());
    assert_ne!(ep_a, ep_b);

    let out2 = store
      .replace(CatalogSnapshot {
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
      })
      .unwrap();
    assert_eq!(out2.exports.len(), 1);
    assert_eq!(out2.exports[0].endpoint_id, ep_a);
    assert_eq!(out2.exports[0].name, "A renamed");
    assert!(!out2.exports[0].enabled);
  }

  #[test]
  fn patch_and_delete_crud() {
    let dir = tempdir().unwrap();
    let mut store = CatalogStore::new(dir.path(), "ThatsMatter");
    let created = store.create(sample_create("Lamp", "light.lamp")).unwrap();
    let ep = created.endpoint_id;

    let patched = store
      .patch(
        created.export_id,
        PatchExport {
          name: Some("Kitchen Lamp".into()),
          enabled: Some(false),
          ..Default::default()
        },
      )
      .unwrap();
    assert_eq!(patched.name, "Kitchen Lamp");
    assert!(!patched.enabled);
    assert_eq!(patched.endpoint_id, ep);

    store.delete(created.export_id).unwrap();
    assert!(store.get(created.export_id).is_none());
  }

  #[test]
  fn rejects_empty_name() {
    let dir = tempdir().unwrap();
    let mut store = CatalogStore::new(dir.path(), "ThatsMatter");
    let err = store
      .create(CreateExport {
        export_id: None,
        name: "".into(),
        type_: DeviceType::Contact,
        primary_entity_id: "binary_sensor.door".into(),
        linked: BTreeMap::new(),
        area_id: None,
        enabled: true,
      })
      .unwrap_err();
    assert!(matches!(err, CatalogError::Invalid(_)));
  }
}
