//! Commissionable Matter backend powered by `rs-matter` / `rs-matter-stack`.
//!
//! Publishes a Matter bridge: endpoint 0 is the root node, endpoint 1 the
//! aggregator, and every enabled OnOff-capable export gets its own bridged
//! endpoint at catalog `endpoint_id` + 1. Controllers (HA Matter Server,
//! chip-tool, Alexa, …) commission using this install's pairing material and
//! receive subscription reports as HA state changes.
//!
//! Pairing-window truth lives on [`ExportPlane`]: rs-matter opens a 900s basic
//! window at startup only when no fabrics exist, and does not expose a
//! read-only probe. Open/close from IPC is executed on the stack thread via a
//! request mailbox in the plane `run()` loop.
//!
//! Networking: [`LanNetifs`] exposes a single Matterbridge-style LAN face so
//! multi-NIC HAOS hosts do not bind Docker/hassio virtual interfaces. mDNS
//! prefers system Avahi (D-Bus) and falls back to Zeroconf.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::{self, JoinHandle};

use async_trait::async_trait;
use parking_lot::Mutex;
use uuid::Uuid;

use super::backend::{MatterBackend, clamp_pairing_timeout};
use super::commissioning::CommissioningMaterial;
use super::export_plane::{BridgedEndpointMatcher, ExportPlane};
use super::lan_netif::LanNetifs;
use super::pairing::{basic_comm_data, pairing_material_for};
use crate::catalog::{CommandRequest, Export, HaStateValue, PairingMaterial};
use crate::config::BackendKind;

/// Production Matter backend: commissionable IP bridge.
pub struct RsMatterBackend {
  data_dir: PathBuf,
  /// Optional LAN interface pin (`eth0`, `enp1s0`, …); `None` = auto-select.
  mdns_interface: Option<String>,
  /// Bridged endpoint table, shared with the Matter stack thread.
  plane: Arc<ExportPlane>,
  running: AtomicBool,
  error: Mutex<Option<String>>,
  /// Matter stack thread (keeps process advertising while join handle is alive).
  _stack_thread: Mutex<Option<JoinHandle<()>>>,
  commissioning: CommissioningMaterial,
  pairing: PairingMaterial,
}

impl RsMatterBackend {
  pub fn new(data_dir: impl Into<PathBuf>, mdns_interface: Option<String>) -> anyhow::Result<Self> {
    let data_dir = data_dir.into();
    let commissioning = CommissioningMaterial::load_or_generate(&data_dir)?;
    let mdns_interface = mdns_interface.map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
    Ok(Self {
      data_dir,
      mdns_interface,
      plane: Arc::new(ExportPlane::new()),
      running: AtomicBool::new(false),
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
    let mdns_interface = self.mdns_interface.clone();
    let (ready_tx, ready_rx) = std::sync::mpsc::channel::<Result<(), String>>();

    let handle = thread::Builder::new()
      .name("thatsmatter-matter".into())
      .spawn(move || {
        if let Err(err) = run_matter_stack(data_dir, commissioning, plane, mdns_interface, ready_tx) {
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

/// Thin stack `Mdns` adapter around `rs-matter`'s Avahi backend.
///
/// Implemented here so we can enable `zbus` on `rs-matter` without also enabling
/// `rs-matter-stack`'s `zbus` feature (which pulls a Linux-only bluez path).
struct AvahiMdnsService {
  inner: rs_matter_stack::matter::transport::network::mdns::avahi::AvahiMdns,
}

impl rs_matter_stack::mdns::Mdns for AvahiMdnsService {
  async fn run<C, U>(
    &mut self,
    matter: &rs_matter_stack::matter::Matter<'_>,
    _crypto: C,
    _udp: U,
    _mac: &[u8],
    _ipv4: core::net::Ipv4Addr,
    _ipv6: core::net::Ipv6Addr,
    _interface: u32,
  ) -> Result<(), rs_matter_stack::matter::error::Error>
  where
    C: rs_matter_stack::matter::crypto::Crypto,
    U: edge_nal::UdpBind,
  {
    self.inner.run(matter).await
  }
}

fn run_matter_stack(
  data_dir: PathBuf,
  commissioning: CommissioningMaterial,
  plane: Arc<ExportPlane>,
  mdns_interface: Option<String>,
  ready_tx: std::sync::mpsc::Sender<Result<(), String>>,
) -> Result<(), String> {
  use core::mem::MaybeUninit;

  use rs_matter_stack::eth::EthMatterStack;
  use rs_matter_stack::matter::crypto::default_crypto;
  use rs_matter_stack::matter::dm::EmptyHandler;
  use rs_matter_stack::matter::dm::devices::test::{DAC_PRIVKEY, TEST_DEV_ATT, TEST_DEV_DET};
  use rs_matter_stack::matter::persist::DirKvBlobStore;
  use rs_matter_stack::matter::transport::network::mdns::avahi::AvahiMdns;
  use rs_matter_stack::matter::transport::network::mdns::zeroconf::ZeroconfMdns;
  use rs_matter_stack::matter::utils::init::InitMaybeUninit;
  use rs_matter_stack::matter::utils::zbus::Connection;

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

    // Stack opens a 900s basic window when fabric count is 0; mirror that into
    // our deadline so `/status.pairing_open` is truthful from first poll.
    let fabric_count = stack.matter().with_state(|state| state.fabrics.iter().count());
    let fabric_count = u8::try_from(fabric_count).unwrap_or(u8::MAX);
    plane.note_startup_commissioning_state(fabric_count);

    let kv = stack.matter().kv(store);

    let _ = stack
      .matter()
      .print_standard_qr_text(rs_matter_stack::matter::pairing::DiscoveryCapabilities::IP);

    // Matterbridge-style LAN filter: single best real face (or pin).
    let lan = LanNetifs::new(mdns_interface.clone());
    lan.log_inventory();

    let _ = ready_tx.send(Ok(()));

    // Prefer system Avahi (ignores per-iface MAC/IPv4; multi-homes via the daemon).
    // Fall back to Zeroconf when D-Bus/Avahi is unavailable (e.g. macOS dev).
    match futures_lite::future::block_on(Connection::system()) {
      Ok(conn) => {
        tracing::info!("mDNS: using Avahi via system D-Bus");
        let matter = core::pin::pin!(stack.run_preex(
          edge_nal_std::Stack::new(),
          LanNetifs::new(mdns_interface),
          AvahiMdnsService {
            inner: AvahiMdns::new(conn),
          },
          &crypto,
          (plane.as_ref(), handler),
          kv,
          (),
        ));
        futures_lite::future::block_on(matter).map_err(|e| format!("run: {e:?}"))
      }
      Err(err) => {
        tracing::warn!(
          error = %err,
          "mDNS: Avahi system bus unavailable; falling back to Zeroconf"
        );
        let matter = core::pin::pin!(stack.run_preex(
          edge_nal_std::Stack::new(),
          LanNetifs::new(mdns_interface),
          ZeroconfMdns::new(),
          &crypto,
          (plane.as_ref(), handler),
          kv,
          (),
        ));
        futures_lite::future::block_on(matter).map_err(|e| format!("run: {e:?}"))
      }
    }
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
    tracing::info!(
      data_dir = %self.data_dir.display(),
      setup_code = %self.pairing.setup_code,
      pairing_open = self.plane.pairing_open(),
      fabrics = self.plane.commissioned_fabrics(),
      mdns_interface = ?self.mdns_interface,
      "RsMatterBackend started (commissionable IP bridge)"
    );
    Ok(())
  }

  async fn is_running(&self) -> bool {
    self.running.load(Ordering::SeqCst)
  }

  async fn pairing_open(&self) -> bool {
    self.plane.pairing_open()
  }

  async fn open_pairing_window(&self, timeout_secs: u16) -> anyhow::Result<()> {
    if !self.running.load(Ordering::SeqCst) {
      anyhow::bail!("Matter stack is not running");
    }
    let timeout = clamp_pairing_timeout(timeout_secs);
    self.plane.request_open_window(timeout)
  }

  async fn close_pairing_window(&self) -> anyhow::Result<()> {
    if !self.running.load(Ordering::SeqCst) {
      anyhow::bail!("Matter stack is not running");
    }
    self.plane.request_close_window()
  }

  async fn commissioned_fabrics(&self) -> u8 {
    self.plane.commissioned_fabrics()
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
