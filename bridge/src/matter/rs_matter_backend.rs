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
//! prefers system Avahi after a live D-Bus probe and falls back to Zeroconf
//! (both need the Avahi daemon on typical Linux/HAOS).
//!
//! # Readiness honesty
//!
//! `ready_tx` is signaled only after UDP 5540 preflight, `stack.startup`, LAN
//! validation, and mDNS backend selection — **immediately before**
//! `block_on(run_preex)`. Residual race: ready means "about to enter run", not
//! that Matter UDP/mDNS sockets are already bound. If `run_preex` later exits
//! (error or unexpected return), shared `running`/`error` are updated so
//! `/status` stops claiming a healthy stack.

use std::io::ErrorKind;
use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr, UdpSocket};
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

/// Matter accessory UDP port (CSA).
const MATTER_UDP_PORT: u16 = 5540;

/// Production Matter backend: commissionable IP bridge.
pub struct RsMatterBackend {
  data_dir: PathBuf,
  /// Optional LAN interface pin (`eth0`, `enp1s0`, …); `None` = auto-select.
  mdns_interface: Option<String>,
  /// Bridged endpoint table, shared with the Matter stack thread.
  plane: Arc<ExportPlane>,
  /// Shared with the stack thread so death after ready clears `/status.running`.
  running: Arc<AtomicBool>,
  /// Shared with the stack thread so unexpected exit surfaces on `/status.error`.
  error: Arc<Mutex<Option<String>>>,
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
      running: Arc::new(AtomicBool::new(false)),
      error: Arc::new(Mutex::new(None)),
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
    let running = Arc::clone(&self.running);
    let error = Arc::clone(&self.error);
    let (ready_tx, ready_rx) = std::sync::mpsc::channel::<Result<(), String>>();

    let handle = thread::Builder::new()
      .name("thatsmatter-matter".into())
      .spawn(move || {
        if let Err(err) = run_matter_stack(data_dir, commissioning, plane, mdns_interface, running, error, ready_tx) {
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
        self.running.store(false, Ordering::SeqCst);
        anyhow::bail!("Matter stack failed to start: {msg}");
      }
      Err(_) => {
        let msg = "Matter stack start timed out".to_string();
        *self.error.lock() = Some(msg.clone());
        self.running.store(false, Ordering::SeqCst);
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

/// Briefly probe UDP 5540 on IPv6 and IPv4 so we fail fast if Matterbridge (or
/// another Matter accessory) already owns the port.
///
/// Sockets are dropped before the stack binds. Residual race: another process
/// can grab 5540 between this probe and `run_preex` bind.
fn preflight_matter_udp_port() -> Result<(), String> {
  // Probe IPv6 first (Matter prefers dual-stack / IPv6 bind).
  match UdpSocket::bind(SocketAddr::from((Ipv6Addr::UNSPECIFIED, MATTER_UDP_PORT))) {
    Ok(sock) => drop(sock),
    Err(err) if err.kind() == ErrorKind::AddrInUse => {
      return Err(format!(
        "UDP port {MATTER_UDP_PORT} already in use (IPv6). Stop Matterbridge or any other Matter \
         process that binds 5540, then restart ThatsMatter."
      ));
    }
    Err(err) => {
      // e.g. IPv6 disabled on the host — still try IPv4.
      tracing::debug!(error = %err, "UDP {MATTER_UDP_PORT} IPv6 preflight skipped");
    }
  }

  match UdpSocket::bind(SocketAddr::from((Ipv4Addr::UNSPECIFIED, MATTER_UDP_PORT))) {
    Ok(sock) => drop(sock),
    Err(err) if err.kind() == ErrorKind::AddrInUse => {
      return Err(format!(
        "UDP port {MATTER_UDP_PORT} already in use (IPv4). Stop Matterbridge or any other Matter \
         process that binds 5540, then restart ThatsMatter."
      ));
    }
    Err(err) => {
      tracing::debug!(error = %err, "UDP {MATTER_UDP_PORT} IPv4 preflight skipped");
    }
  }

  Ok(())
}

/// Confirm Avahi is actually answering on the system bus (not just that the bus exists).
async fn probe_avahi(conn: &rs_matter_stack::matter::utils::zbus::Connection) -> Result<String, String> {
  use rs_matter_stack::matter::utils::zbus_proxies::avahi::server2::Server2Proxy;

  let proxy = Server2Proxy::new(conn)
    .await
    .map_err(|err| format!("Avahi Server2 proxy: {err}"))?;
  let version = proxy
    .get_version_string()
    .await
    .map_err(|err| format!("Avahi GetVersionString: {err}"))?;
  Ok(version)
}

fn run_matter_stack(
  data_dir: PathBuf,
  commissioning: CommissioningMaterial,
  plane: Arc<ExportPlane>,
  mdns_interface: Option<String>,
  running: Arc<AtomicBool>,
  error: Arc<Mutex<Option<String>>>,
  ready_tx: std::sync::mpsc::Sender<Result<(), String>>,
) -> Result<(), String> {
  use core::mem::MaybeUninit;

  use rs_matter_stack::eth::EthMatterStack;
  use rs_matter_stack::matter::crypto::default_crypto;
  use rs_matter_stack::matter::dm::EmptyHandler;
  use rs_matter_stack::matter::dm::devices::test::{DAC_PRIVKEY, TEST_DEV_ATT, TEST_DEV_DET};
  use rs_matter_stack::matter::persist::DirKvBlobStore;
  use rs_matter_stack::matter::transport::network::mdns::avahi::AvahiMdns;
  use rs_matter_stack::matter::transport::network::mdns::builtin::BuiltinMdns;
  use rs_matter_stack::matter::utils::init::InitMaybeUninit;
  use rs_matter_stack::matter::utils::zbus::Connection;

  const BUMP_SIZE: usize = 23500;

  // Set true only after ready_tx Ok is sent (visible after the closure returns).
  let ready_sent = AtomicBool::new(false);

  let result = (|| -> Result<(), String> {
    // Fail fast if another Matter accessory owns the transport port.
    preflight_matter_udp_port()?;

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
    lan.validate_for_start()?;

    // mDNS: prefer system Avahi when the daemon is on the host bus. On HAOS the
    // Avahi bus name is often missing (`ServiceUnknown: not activatable`); the
    // Zeroconf crate still needs Avahi on Linux and then "registers" while
    // advertising nothing useful. Fall back to rs-matter's **BuiltinMdns**
    // (UDP 5353, Matterbridge-style self-contained advertisement).
    enum MdnsChoice {
      Avahi(rs_matter_stack::matter::utils::zbus::Connection),
      Builtin,
    }

    let mdns_choice = match futures_lite::future::block_on(Connection::system()) {
      Ok(conn) => match futures_lite::future::block_on(probe_avahi(&conn)) {
        Ok(version) => {
          tracing::info!(%version, "mDNS backend: Avahi (GetVersionString probe ok)");
          MdnsChoice::Avahi(conn)
        }
        Err(probe_err) => {
          tracing::warn!(
            error = %probe_err,
            "mDNS: Avahi not usable on host D-Bus; using BuiltinMdns (UDP 5353)"
          );
          MdnsChoice::Builtin
        }
      },
      Err(err) => {
        tracing::warn!(
          error = %err,
          "mDNS: system D-Bus unavailable; using BuiltinMdns (UDP 5353)"
        );
        MdnsChoice::Builtin
      }
    };

    // Ready: preflight + startup + LAN validate + mDNS selection done.
    // Residual race: run_preex has not bound sockets yet.
    running.store(true, Ordering::SeqCst);
    let _ = ready_tx.send(Ok(()));
    ready_sent.store(true, Ordering::SeqCst);

    match mdns_choice {
      MdnsChoice::Avahi(conn) => {
        tracing::info!("mDNS: active backend = Avahi (system D-Bus)");
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
      MdnsChoice::Builtin => {
        tracing::info!("mDNS: active backend = BuiltinMdns (self-contained UDP 5353)");
        let matter = core::pin::pin!(stack.run_preex(
          edge_nal_std::Stack::new(),
          LanNetifs::new(mdns_interface),
          BuiltinMdns::new(),
          &crypto,
          (plane.as_ref(), handler),
          kv,
          (),
        ));
        futures_lite::future::block_on(matter).map_err(|e| format!("run: {e:?}"))
      }
    }
  })();

  // Stack left run (or failed before ready): never leave /status claiming healthy.
  running.store(false, Ordering::SeqCst);
  let was_ready = ready_sent.load(Ordering::SeqCst);
  match &result {
    Err(err) => {
      *error.lock() = Some(err.clone());
      if !was_ready {
        let _ = ready_tx.send(Err(err.clone()));
      } else {
        tracing::error!(error = %err, "Matter stack run exited after ready; status will show not running");
      }
    }
    Ok(()) if was_ready => {
      let msg = "Matter stack exited unexpectedly".to_string();
      *error.lock() = Some(msg.clone());
      tracing::error!("{msg}");
    }
    Ok(()) => {}
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
    // Clear a stale error from a previous failed start attempt.
    *self.error.lock() = None;
    self.spawn_stack()?;
    // `running` is set true by the stack thread when it signals ready.
    tracing::info!(
      data_dir = %self.data_dir.display(),
      setup_code = %self.pairing.setup_code,
      pairing_open = self.plane.pairing_open(),
      fabrics = self.plane.commissioned_fabrics(),
      mdns_interface = ?self.mdns_interface,
      running = self.running.load(Ordering::SeqCst),
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
