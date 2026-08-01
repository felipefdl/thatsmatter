//! Commissionable Matter backend powered by `rs-matter` / `rs-matter-stack`.
//!
//! Publishes a Matter bridge: endpoint 0 is the root node, endpoint 1 the
//! aggregator, and every enabled OnOff-capable export gets its own bridged
//! endpoint at catalog `endpoint_id` + 1. Controllers (HA Matter Server,
//! chip-tool, Alexa, …) commission using this install's pairing material and
//! receive subscription reports as HA state changes.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::{self, JoinHandle};

use async_trait::async_trait;
use parking_lot::Mutex;
use uuid::Uuid;

use super::backend::MatterBackend;
use super::commissioning::CommissioningMaterial;
use super::export_plane::{BridgedEndpointMatcher, ExportPlane};
use super::pairing::{basic_comm_data, pairing_material_for};
use crate::catalog::{CommandRequest, Export, HaStateValue, PairingMaterial};
use crate::config::BackendKind;

/// Production Matter backend: commissionable IP bridge.
pub struct RsMatterBackend {
  data_dir: PathBuf,
  /// Bridged endpoint table, shared with the Matter stack thread.
  plane: Arc<ExportPlane>,
  running: AtomicBool,
  pairing_open: AtomicBool,
  error: Mutex<Option<String>>,
  /// Matter stack thread (keeps process advertising while join handle is alive).
  _stack_thread: Mutex<Option<JoinHandle<()>>>,
  commissioning: CommissioningMaterial,
  pairing: PairingMaterial,
}

impl RsMatterBackend {
  pub fn new(data_dir: impl Into<PathBuf>) -> anyhow::Result<Self> {
    let data_dir = data_dir.into();
    let commissioning = CommissioningMaterial::load_or_generate(&data_dir)?;
    Ok(Self {
      data_dir,
      plane: Arc::new(ExportPlane::new()),
      running: AtomicBool::new(false),
      pairing_open: AtomicBool::new(false),
      error: Mutex::new(None),
      _stack_thread: Mutex::new(None),
      pairing: pairing_material_for(&commissioning),
      commissioning,
    })
  }

  fn spawn_stack(&self) -> anyhow::Result<()> {
    let data_dir = self.data_dir.clone();
    let commissioning = self.commissioning;
    let plane = Arc::clone(&self.plane);
    let (ready_tx, ready_rx) = std::sync::mpsc::channel::<Result<(), String>>();

    let handle = thread::Builder::new()
      .name("thatsmatter-matter".into())
      .spawn(move || {
        if let Err(err) = run_matter_stack(data_dir, commissioning, plane, ready_tx) {
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
  commissioning: CommissioningMaterial,
  plane: Arc<ExportPlane>,
  ready_tx: std::sync::mpsc::Sender<Result<(), String>>,
) -> Result<(), String> {
  use core::mem::MaybeUninit;

  use rs_matter_stack::eth::EthMatterStack;
  use rs_matter_stack::matter::crypto::default_crypto;
  use rs_matter_stack::matter::dm::EmptyHandler;
  use rs_matter_stack::matter::dm::devices::test::{DAC_PRIVKEY, TEST_DEV_ATT, TEST_DEV_DET};
  use rs_matter_stack::matter::dm::networks::unix::UnixNetifs;
  use rs_matter_stack::matter::persist::DirKvBlobStore;
  use rs_matter_stack::matter::transport::network::mdns::zeroconf::ZeroconfMdns;
  use rs_matter_stack::matter::utils::init::InitMaybeUninit;

  const BUMP_SIZE: usize = 23500;

  let result = (|| -> Result<(), String> {
    // Heap-allocated and initialized in place: the stack is tens of KB, and
    // leaking it is what gives the `&'static` borrow `run_preex` needs.
    let uninit: &'static mut MaybeUninit<EthMatterStack<BUMP_SIZE, ()>> = Box::leak(Box::new_uninit());
    // Attestation stays on the CSA test credentials (TEST_DEV_DET keeps VID 0xFFF1 / PID 0x8001,
    // which the example CD is bound to); only the pairing material is per install.
    let stack = uninit.init_with(EthMatterStack::init(
      &TEST_DEV_DET,
      basic_comm_data(&commissioning),
      &TEST_DEV_ATT,
    ));

    let crypto = default_crypto(rand::thread_rng(), DAC_PRIVKEY);

    // `rs-matter-stack` serves endpoint 0 from its own chain and forwards
    // everything else here, so one catch-all link carries the whole bridge.
    let handler = EmptyHandler.chain(BridgedEndpointMatcher, plane.as_ref());

    std::fs::create_dir_all(&data_dir).map_err(|e| e.to_string())?;
    let store_path = data_dir.join("rs-matter");
    std::fs::create_dir_all(&store_path).map_err(|e| e.to_string())?;

    let mut store = DirKvBlobStore::new(store_path);
    futures_lite::future::block_on(stack.startup(&crypto, &mut store)).map_err(|e| format!("startup: {e:?}"))?;

    let kv = stack.matter().kv(store);

    let _ = stack
      .matter()
      .print_standard_qr_text(rs_matter_stack::matter::pairing::DiscoveryCapabilities::IP);

    let _ = ready_tx.send(Ok(()));

    let matter = core::pin::pin!(stack.run_preex(
      edge_nal_std::Stack::new(),
      UnixNetifs,
      ZeroconfMdns::new(),
      &crypto,
      (plane.as_ref(), handler),
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
      "RsMatterBackend started (commissionable IP bridge)"
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
    self.plane.set_exports(exports);
    tracing::info!(
      total = exports.len(),
      endpoints = ?self.plane.endpoint_ids(),
      "bridged endpoints rebuilt"
    );
    Ok(())
  }

  async fn apply_state(&self, export_id: Uuid, states: &[HaStateValue]) -> anyhow::Result<u32> {
    Ok(self.plane.apply_state(export_id, states))
  }

  async fn take_commands(&self) -> Vec<CommandRequest> {
    self.plane.take_commands()
  }

  async fn pairing_info(&self) -> PairingMaterial {
    self.pairing.clone()
  }

  async fn status_error(&self) -> Option<String> {
    self.error.lock().clone()
  }
}
