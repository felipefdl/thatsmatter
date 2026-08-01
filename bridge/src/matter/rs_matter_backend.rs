//! Commissionable Matter backend powered by `rs-matter` / `rs-matter-stack`.
//!
//! Runs an Ethernet (IP) OnOff light endpoint that tracks the primary enabled
//! OnOff export from the catalog. Controllers (HA Matter Server, chip-tool,
//! Alexa, etc.) can commission using the test-device pairing material.

use std::cell::RefCell;
use std::collections::{BTreeMap, VecDeque};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::{self, JoinHandle};

use async_trait::async_trait;
use parking_lot::Mutex;
use uuid::Uuid;

use super::backend::MatterBackend;
use super::on_off_map::{on_off_command, on_off_from_states, primary_on_off_export};
use super::pairing::test_device_pairing_material;
use crate::catalog::{CommandRequest, Export, HaStateValue, PairingMaterial};
use crate::config::BackendKind;

/// Endpoint id for the single OnOff light (root is 0).
const LIGHT_ENDPOINT_ID: u16 = 1;

/// Shared OnOff + command state between the Matter stack thread and the IPC plane.
struct SharedLight {
  on: AtomicBool,
  /// Suppress command enqueue when state is applied from HA.
  from_ha: AtomicBool,
  export_id: Mutex<Option<Uuid>>,
  commands: Mutex<VecDeque<CommandRequest>>,
  exports: Mutex<Vec<Export>>,
  entity_state: Mutex<BTreeMap<String, HaStateValue>>,
}

impl SharedLight {
  fn new() -> Self {
    Self {
      on: AtomicBool::new(false),
      from_ha: AtomicBool::new(false),
      export_id: Mutex::new(None),
      commands: Mutex::new(VecDeque::new()),
      exports: Mutex::new(Vec::new()),
      entity_state: Mutex::new(BTreeMap::new()),
    }
  }

  fn set_from_controller(&self, on: bool) {
    self.on.store(on, Ordering::SeqCst);
    if self.from_ha.load(Ordering::SeqCst) {
      return;
    }
    let Some(export_id) = *self.export_id.lock() else {
      tracing::debug!("OnOff from controller but no export bound yet");
      return;
    };
    self.commands.lock().push_back(on_off_command(export_id, on));
    tracing::info!(%export_id, on, "Matter controller OnOff → command queue");
  }

  fn set_from_ha(&self, on: bool) {
    self.from_ha.store(true, Ordering::SeqCst);
    self.on.store(on, Ordering::SeqCst);
    self.from_ha.store(false, Ordering::SeqCst);
  }
}

/// OnOff hooks that bridge Matter cluster writes into our command queue.
struct ExportOnOffHooks {
  shared: Arc<SharedLight>,
}

impl rs_matter_stack::matter::dm::clusters::app::on_off::OnOffHooks for ExportOnOffHooks {
  // Same cluster metadata as TestOnOffDeviceLogic (Lighting feature OnOff).
  #[allow(clippy::needless_update)]
  const CLUSTER: rs_matter_stack::matter::dm::Cluster<'static> =
    rs_matter_stack::matter::dm::clusters::app::on_off::test::TestOnOffDeviceLogic::CLUSTER;

  fn on_off(&self) -> bool {
    self.shared.on.load(Ordering::SeqCst)
  }

  fn set_on_off(&self, on: bool) {
    self.shared.set_from_controller(on);
  }

  fn start_up_on_off(
    &self,
  ) -> rs_matter_stack::matter::tlv::Nullable<rs_matter_stack::matter::dm::clusters::app::on_off::StartUpOnOffEnum> {
    rs_matter_stack::matter::tlv::Nullable::none()
  }

  fn set_start_up_on_off(
    &self,
    _value: rs_matter_stack::matter::tlv::Nullable<
      rs_matter_stack::matter::dm::clusters::app::on_off::StartUpOnOffEnum,
    >,
  ) -> Result<(), rs_matter_stack::matter::error::Error> {
    Ok(())
  }

  async fn handle_off_with_effect(
    &self,
    _effect: rs_matter_stack::matter::dm::clusters::app::on_off::EffectVariantEnum,
  ) {
  }

  async fn run<F: Fn(rs_matter_stack::matter::dm::clusters::app::on_off::OutOfBandMessage)>(&self, _notify: F) {
    core::future::pending::<()>().await;
  }
}

/// Production Matter backend: commissionable IP OnOff device.
pub struct RsMatterBackend {
  data_dir: PathBuf,
  shared: Arc<SharedLight>,
  running: AtomicBool,
  pairing_open: AtomicBool,
  error: Mutex<Option<String>>,
  /// Matter stack thread (keeps process advertising while join handle is alive).
  _stack_thread: Mutex<Option<JoinHandle<()>>>,
  pairing: PairingMaterial,
}

impl RsMatterBackend {
  pub fn new(data_dir: impl Into<PathBuf>) -> Self {
    Self {
      data_dir: data_dir.into(),
      shared: Arc::new(SharedLight::new()),
      running: AtomicBool::new(false),
      pairing_open: AtomicBool::new(false),
      error: Mutex::new(None),
      _stack_thread: Mutex::new(None),
      pairing: test_device_pairing_material(),
    }
  }

  fn spawn_stack(&self) -> anyhow::Result<()> {
    let data_dir = self.data_dir.clone();
    let shared = Arc::clone(&self.shared);
    let (ready_tx, ready_rx) = std::sync::mpsc::channel::<Result<(), String>>();

    let handle = thread::Builder::new()
      .name("thatsmatter-matter".into())
      .spawn(move || {
        if let Err(err) = run_matter_stack(data_dir, shared, ready_tx) {
          tracing::error!(error = %err, "Matter stack thread exited with error");
        }
      })?;

    match ready_rx.recv_timeout(std::time::Duration::from_secs(30)) {
      Ok(Ok(())) => {
        *self._stack_thread.lock() = Some(handle);
        Ok(())
      }
      Ok(Err(msg)) => {
        *self.error.lock() = Some(msg.clone());
        anyhow::bail!("Matter stack failed to start: {msg}");
      }
      Err(_) => {
        let msg = "Matter stack start timed out".to_string();
        *self.error.lock() = Some(msg.clone());
        anyhow::bail!(msg);
      }
    }
  }
}

fn run_matter_stack(
  data_dir: PathBuf,
  shared: Arc<SharedLight>,
  ready_tx: std::sync::mpsc::Sender<Result<(), String>>,
) -> Result<(), String> {
  use rs_matter_stack::eth::EthMatterStack;
  use rs_matter_stack::matter::crypto::{Crypto, default_crypto};
  use rs_matter_stack::matter::dm::clusters::app::on_off;
  use rs_matter_stack::matter::dm::clusters::app::on_off::OnOffHooks;
  use rs_matter_stack::matter::dm::clusters::app::on_off::test::TestOnOffDeviceLogic;
  use rs_matter_stack::matter::dm::clusters::desc;
  use rs_matter_stack::matter::dm::clusters::desc::ClusterHandler as _;
  use rs_matter_stack::matter::dm::devices::DEV_TYPE_ON_OFF_LIGHT;
  use rs_matter_stack::matter::dm::devices::test::{DAC_PRIVKEY, TEST_DEV_ATT, TEST_DEV_COMM, TEST_DEV_DET};
  use rs_matter_stack::matter::dm::networks::unix::UnixNetifs;
  use rs_matter_stack::matter::dm::{Async, Dataver, EmptyHandler, Endpoint, EpClMatcher, Node};
  use rs_matter_stack::matter::persist::DirKvBlobStore;
  use rs_matter_stack::matter::transport::network::mdns::zeroconf::ZeroconfMdns;
  use rs_matter_stack::matter::utils::init::InitMaybeUninit;
  use rs_matter_stack::matter::{clusters, devices};
  use static_cell::StaticCell;

  const BUMP_SIZE: usize = 23500;

  // Initialize once per process (binary runs a single backend instance).
  static MATTER_STACK: StaticCell<EthMatterStack<BUMP_SIZE, ()>> = StaticCell::new();

  let result = (|| -> Result<(), String> {
    let stack = MATTER_STACK
      .uninit()
      .init_with(EthMatterStack::init(&TEST_DEV_DET, TEST_DEV_COMM, &TEST_DEV_ATT));

    let crypto = default_crypto(rand::thread_rng(), DAC_PRIVKEY);
    let mut rand_src = crypto.weak_rand().map_err(|e| format!("rand: {e:?}"))?;

    let hooks = ExportOnOffHooks {
      shared: Arc::clone(&shared),
    };
    let on_off = on_off::OnOffHandler::new_standalone(Dataver::new_rand(&mut rand_src), LIGHT_ENDPOINT_ID, hooks);

    let handler = EmptyHandler
      .chain(
        EpClMatcher::new(Some(LIGHT_ENDPOINT_ID), Some(TestOnOffDeviceLogic::CLUSTER.id)),
        on_off::HandlerAsyncAdaptor(&on_off),
      )
      .chain(
        EpClMatcher::new(Some(LIGHT_ENDPOINT_ID), Some(desc::DescHandler::CLUSTER.id)),
        Async(desc::DescHandler::new(Dataver::new_rand(&mut rand_src)).adapt()),
      );

    std::fs::create_dir_all(&data_dir).map_err(|e| e.to_string())?;
    let store_path = data_dir.join("rs-matter");
    std::fs::create_dir_all(&store_path).map_err(|e| e.to_string())?;

    let mut store = DirKvBlobStore::new(store_path);
    futures_lite::future::block_on(stack.startup(&crypto, &mut store)).map_err(|e| format!("startup: {e:?}"))?;

    let kv = stack.matter().kv(store);

    let _ = stack
      .matter()
      .print_standard_qr_text(rs_matter_stack::matter::pairing::DiscoveryCapabilities::IP);

    // Match light_eth example: const cluster metadata for the OnOff light endpoint.
    const NODE: Node = Node {
      endpoints: &[
        EthMatterStack::<0, ()>::root_endpoint(),
        Endpoint::new(
          LIGHT_ENDPOINT_ID,
          devices!(DEV_TYPE_ON_OFF_LIGHT),
          clusters!(desc::DescHandler::CLUSTER, TestOnOffDeviceLogic::CLUSTER),
        ),
      ],
    };

    let _ = ready_tx.send(Ok(()));

    let matter = core::pin::pin!(stack.run_preex(
      edge_nal_std::Stack::new(),
      UnixNetifs,
      ZeroconfMdns::new(),
      &crypto,
      (NODE, handler),
      kv,
      (),
    ));

    futures_lite::future::block_on(matter).map_err(|e| format!("run: {e:?}"))
  })();

  if let Err(ref err) = result {
    let _ = ready_tx.send(Err(err.clone()));
  }
  result
}

#[async_trait]
impl MatterBackend for RsMatterBackend {
  fn kind(&self) -> BackendKind {
    BackendKind::RsMatter
  }

  async fn start(&self) -> anyhow::Result<()> {
    if self.running.load(Ordering::SeqCst) {
      return Ok(());
    }
    std::fs::create_dir_all(&self.data_dir)?;
    self.spawn_stack()?;
    self.running.store(true, Ordering::SeqCst);
    self.pairing_open.store(true, Ordering::SeqCst);
    tracing::info!(
      data_dir = %self.data_dir.display(),
      setup_code = %self.pairing.setup_code,
      "RsMatterBackend started (commissionable IP OnOff)"
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
    *self.shared.exports.lock() = exports.to_vec();
    if let Some(primary) = primary_on_off_export(exports) {
      *self.shared.export_id.lock() = Some(primary.export_id);
      tracing::info!(
        export_id = %primary.export_id,
        name = %primary.name,
        endpoint_id = ?primary.endpoint_id,
        "OnOff export bound to Matter endpoint {LIGHT_ENDPOINT_ID}"
      );
    } else {
      *self.shared.export_id.lock() = None;
      tracing::info!("no enabled OnOff export; Matter light unbound");
    }
    Ok(())
  }

  async fn apply_state(&self, export_id: Uuid, states: &[HaStateValue]) -> anyhow::Result<u32> {
    let exports = self.shared.exports.lock().clone();
    let Some(exp) = exports.iter().find(|e| e.export_id == export_id) else {
      return Ok(0);
    };
    let mut applied = 0u32;
    {
      let mut map = self.shared.entity_state.lock();
      for st in states {
        map.insert(st.entity_id.clone(), st.clone());
        applied += 1;
      }
    }
    if let Some(on) = on_off_from_states(exp, states) {
      self.shared.set_from_ha(on);
      tracing::debug!(%export_id, on, "HA state applied to Matter OnOff");
    }
    Ok(applied)
  }

  async fn take_commands(&self) -> Vec<CommandRequest> {
    self.shared.commands.lock().drain(..).collect()
  }

  async fn pairing_info(&self) -> PairingMaterial {
    self.pairing.clone()
  }

  async fn status_error(&self) -> Option<String> {
    self.error.lock().clone()
  }
}

// Silence unused import warning path for RefCell in some feature combos.
#[allow(dead_code)]
type _Hold = RefCell<()>;
