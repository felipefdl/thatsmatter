//! Development Matter backend: full catalog/command API with placeholder pairing.

use std::collections::{BTreeMap, VecDeque};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};

use async_trait::async_trait;
use parking_lot::Mutex;
use uuid::Uuid;

use super::backend::MatterBackend;
use super::on_off_map::{on_off_command, on_off_from_states};
use super::pairing::test_device_pairing_material;
use crate::catalog::{CommandRequest, DeviceType, Export, HaStateValue, PairingMaterial};
use crate::config::BackendKind;

/// Offline Matter backend for IPC/unit tests without starting the transport stack.
///
/// Uses the same commissionable pairing material as the real `rs_matter` backend
/// (CSA test VID/PID/passcode), but does not advertise Matter on the network.
/// Use `RsMatterBackend` for a live commissionable node.
pub struct DevMatterBackend {
  data_dir: PathBuf,
  running: AtomicBool,
  pairing_open: AtomicBool,
  exports: Mutex<Vec<Export>>,
  /// Last known HA state keyed by entity_id.
  entity_state: Mutex<BTreeMap<String, HaStateValue>>,
  commands: Mutex<VecDeque<CommandRequest>>,
  error: Mutex<Option<String>>,
  pairing: PairingMaterial,
}

impl DevMatterBackend {
  pub fn new(data_dir: impl Into<PathBuf>) -> Self {
    Self {
      data_dir: data_dir.into(),
      running: AtomicBool::new(false),
      pairing_open: AtomicBool::new(true),
      exports: Mutex::new(Vec::new()),
      entity_state: Mutex::new(BTreeMap::new()),
      commands: Mutex::new(VecDeque::new()),
      error: Mutex::new(None),
      pairing: test_device_pairing_material(),
    }
  }

  /// Test helper: enqueue a command as if a Matter controller issued it.
  pub fn push_command(&self, cmd: CommandRequest) {
    self.commands.lock().push_back(cmd);
  }

  /// Simulate a controller OnOff write against the bound export (if any).
  pub fn simulate_controller_on_off(&self, export_id: uuid::Uuid, on: bool) {
    self.push_command(on_off_command(export_id, on));
  }

  /// Test / debug helper: last applied state for an entity.
  pub fn entity_state(&self, entity_id: &str) -> Option<HaStateValue> {
    self.entity_state.lock().get(entity_id).cloned()
  }
}

#[async_trait]
impl MatterBackend for DevMatterBackend {
  fn kind(&self) -> BackendKind {
    BackendKind::Dev
  }

  async fn start(&self) -> anyhow::Result<()> {
    std::fs::create_dir_all(&self.data_dir)?;
    self.running.store(true, Ordering::SeqCst);
    // Dev backend keeps pairing window notionally open so HA can always show codes.
    self.pairing_open.store(true, Ordering::SeqCst);
    tracing::info!(
      data_dir = %self.data_dir.display(),
      "DevMatterBackend started (offline IPC; same pairing codes as rs_matter, no network advertise)"
    );
    Ok(())
  }

  async fn is_running(&self) -> bool {
    self.running.load(Ordering::SeqCst)
  }

  async fn pairing_open(&self) -> bool {
    self.pairing_open.load(Ordering::SeqCst)
  }

  async fn set_exports(&self, exports: &[Export]) -> anyhow::Result<()> {
    let on_off: Vec<_> = exports
      .iter()
      .filter(|e| e.enabled && e.type_.is_on_off_capable())
      .map(|e| (e.export_id, e.name.clone(), e.type_, e.endpoint_id))
      .collect();
    tracing::info!(
      total = exports.len(),
      on_off = on_off.len(),
      "DevMatterBackend set_exports"
    );
    for (id, name, type_, ep) in &on_off {
      tracing::debug!(%id, %name, ?type_, ?ep, "would map OnOff endpoint");
    }
    *self.exports.lock() = exports.to_vec();
    Ok(())
  }

  async fn apply_state(&self, export_id: Uuid, states: &[HaStateValue]) -> anyhow::Result<u32> {
    let exports = self.exports.lock();
    let Some(exp) = exports.iter().find(|e| e.export_id == export_id) else {
      tracing::warn!(%export_id, "apply_state for unknown export");
      return Ok(0);
    };
    let mut applied = 0u32;
    let mut map = self.entity_state.lock();
    for st in states {
      tracing::debug!(
        %export_id,
        entity_id = %st.entity_id,
        state = %st.state,
        type_ = ?exp.type_,
        "DevMatterBackend apply_state"
      );
      map.insert(st.entity_id.clone(), st.clone());
      applied += 1;
    }
    if let Some(on) = on_off_from_states(exp, states) {
      tracing::info!(%export_id, on, "DevMatterBackend applied OnOff state from HA");
    }
    Ok(applied)
  }

  async fn take_commands(&self) -> Vec<CommandRequest> {
    let mut q = self.commands.lock();
    q.drain(..).collect()
  }

  async fn pairing_info(&self) -> PairingMaterial {
    self.pairing.clone()
  }

  async fn status_error(&self) -> Option<String> {
    self.error.lock().clone()
  }
}

/// Compile-time reminder that `DeviceType` first-ship set includes OnOff path types.
#[allow(dead_code)]
fn _assert_on_off_types() {
  let _ = [
    DeviceType::Light,
    DeviceType::OnOffSwitch,
    DeviceType::OnOffPlug,
    DeviceType::Outlet,
  ];
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::catalog::CommandKind;
  use tempfile::tempdir;

  #[tokio::test]
  async fn pairing_material_non_empty() {
    let dir = tempdir().unwrap();
    let backend = DevMatterBackend::new(dir.path());
    backend.start().await.unwrap();
    let p = backend.pairing_info().await;
    assert!(!p.setup_code.is_empty());
    assert!(!p.qr_payload.is_empty());
    assert!(p.qr_payload.starts_with("MT:"));
    assert_eq!(p.discriminator, crate::matter::pairing::TEST_DISCRIMINATOR);
    assert_eq!(p.passcode, crate::matter::pairing::TEST_PASSCODE);
    assert!(backend.is_running().await);
    assert!(backend.pairing_open().await);
  }

  #[tokio::test]
  async fn controller_on_off_enqueues_command() {
    let dir = tempdir().unwrap();
    let backend = DevMatterBackend::new(dir.path());
    backend.start().await.unwrap();
    let id = Uuid::new_v4();
    backend.simulate_controller_on_off(id, true);
    let cmds = backend.take_commands().await;
    assert_eq!(cmds.len(), 1);
    assert_eq!(cmds[0].export_id, id);
    assert_eq!(cmds[0].on, Some(true));
  }

  #[tokio::test]
  async fn command_queue_drain() {
    let dir = tempdir().unwrap();
    let backend = DevMatterBackend::new(dir.path());
    backend.start().await.unwrap();
    let id = Uuid::new_v4();
    backend.push_command(CommandRequest {
      export_id: id,
      kind: CommandKind::OnOff,
      on: Some(true),
      level: None,
      position: None,
    });
    let cmds = backend.take_commands().await;
    assert_eq!(cmds.len(), 1);
    assert_eq!(cmds[0].export_id, id);
    assert!(backend.take_commands().await.is_empty());
  }

  #[tokio::test]
  async fn apply_state_counts_and_stores() {
    let dir = tempdir().unwrap();
    let backend = DevMatterBackend::new(dir.path());
    backend.start().await.unwrap();
    let id = Uuid::new_v4();
    let exp = Export {
      export_id: id,
      name: "Lamp".into(),
      type_: DeviceType::Light,
      primary_entity_id: "light.lamp".into(),
      linked: BTreeMap::new(),
      area_id: None,
      enabled: true,
      endpoint_id: Some(1),
    };
    backend.set_exports(&[exp]).await.unwrap();
    let applied = backend
      .apply_state(
        id,
        &[HaStateValue {
          entity_id: "light.lamp".into(),
          state: "on".into(),
          attributes: Default::default(),
        }],
      )
      .await
      .unwrap();
    assert_eq!(applied, 1);
    assert_eq!(backend.entity_state("light.lamp").unwrap().state, "on");
  }
}
