//! Bridged endpoint plane: one Matter endpoint per enabled export.
//!
//! The plane is the only state shared between the IPC/tokio threads and the
//! `rs-matter` stack thread. IPC rebuilds the slot table (`set_exports`) and
//! pushes HA state into it (`apply_state`); the stack thread reads it while
//! serving the data model ([`Metadata`] + [`AsyncHandler`]) and turns queued
//! changes into subscription reports in [`AsyncHandler::run`].
//!
//! Endpoint layout: 0 root (supplied by `rs-matter-stack`), 1 aggregator,
//! catalog `endpoint_id` + 1 for every bridged export.
//!
//! `rs-matter` state is not `Sync` (`Matter`, `Dataver` and the crate's own
//! `Notification` all sit behind a `NoopRawMutex`), so nothing rs-matter owns
//! may be stored here. Data versions are plain atomics and the cross-thread
//! wake-up is a `tokio::sync::Notify`, which is runtime-agnostic and therefore
//! polls fine under `futures_lite::block_on` on the stack thread.

use std::collections::{BTreeSet, VecDeque};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU32, AtomicU64, Ordering};
use std::time::Duration;

use parking_lot::{Mutex, RwLock};
use rs_matter::dm::clusters::decl::bridged_device_basic_information as bridged_info;
use rs_matter::dm::clusters::decl::on_off as on_off_decl;
use rs_matter::dm::clusters::desc;
use rs_matter::dm::clusters::desc::ClusterHandler as _;
use rs_matter::dm::devices::{DEV_TYPE_AGGREGATOR, DEV_TYPE_BRIDGED_NODE, DEV_TYPE_ON_OFF_LIGHT};
use rs_matter::dm::endpoints::ROOT_ENDPOINT_ID;
use rs_matter::dm::{
  AsyncHandler, AttrId, Cluster, ClusterId, Dataver, DeviceType as MatterDeviceType, Endpoint, EndptId, Handler,
  HandlerContext, InvokeContext, InvokeReply, MatchContext, Matcher, Metadata, Node, ReadContext, ReadReply,
  WriteContext,
};
use rs_matter::error::{Error, ErrorCode};
use rs_matter::tlv::{Nullable, TLVBuilderParent, Utf8Str, Utf8StrBuilder};
use rs_matter::with;
use rs_matter_stack::eth::EthMatterStack;
use tokio::sync::Notify;
use uuid::Uuid;

use super::backend::{STARTUP_PAIRING_TIMEOUT_SECS, clamp_pairing_timeout, epoch_secs};
use super::clusters::boolean_state::{self as bool_state_cluster};
use super::clusters::occupancy::{self as occupancy_cluster};
use super::clusters::window_covering::{
  self as window_covering_cluster, CoverMotion, accept_mode_write, ha_cover_from_state, ha_position_to_percent100ths,
  percent100ths_to_ha_position, validate_lift_percent100ths,
};
use super::device_types::{
  DEV_TYPE_CONTACT_SENSOR, DEV_TYPE_OCCUPANCY_SENSOR, DEV_TYPE_ON_OFF_PLUG_IN_UNIT, DEV_TYPE_WINDOW_COVERING,
};
use super::on_off_map::{ha_state_is_on, is_matter_bridged_export, on_off_command, on_off_from_states};
use crate::catalog::{CommandKind, CommandRequest, DeviceType, Export, HaStateValue};

/// Matter endpoint hosting the aggregator (root is 0, bridged devices start at 2).
pub const AGGREGATOR_ENDPOINT_ID: EndptId = 1;

/// How often the stack thread re-samples fabric count when idle.
const FABRIC_SAMPLE_INTERVAL: Duration = Duration::from_secs(2);

/// Cross-thread request to open or close the basic commissioning window.
enum WindowOp {
  Open(u16),
  Close,
}

struct WindowRequest {
  op: WindowOp,
  reply: std::sync::mpsc::Sender<Result<(), String>>,
}

/// Matter caps `NodeLabel` at 32 octets.
const NODE_LABEL_MAX_BYTES: usize = 32;

/// Pending-report bits drained by the stack thread on every wake.
/// Functional cluster attributes (OnOff / BooleanState / Occupancy / WindowCovering).
const REPORT_FUNCTIONAL: u32 = 1 << 0;
const REPORT_NODE_LABEL: u32 = 1 << 1;
/// Bridged-endpoint Descriptor (e.g. DeviceTypeList) after a surface change.
const REPORT_DESCRIPTOR: u32 = 1 << 2;

const DESC_CLUSTER: Cluster<'static> = desc::DescHandler::CLUSTER;

/// Bridged Device Basic Information: mandatory attributes plus `NodeLabel`.
/// Commands and events are dropped — `KeepActive` needs an ICD and the stack is
/// built with a zero-sized event ring buffer.
const BRIDGED_INFO_CLUSTER: Cluster<'static> = bridged_info::FULL_CLUSTER
  .with_attrs(with!(required; bridged_info::AttributeId::NodeLabel))
  .with_cmds(with!())
  .with_events(with!());

/// Base OnOff (no Lighting feature): the `OnOff` attribute and On/Off/Toggle.
const ON_OFF_CLUSTER: Cluster<'static> = on_off_decl::FULL_CLUSTER
  .with_attrs(with!(required))
  .with_cmds(with!(
    on_off_decl::CommandId::Off | on_off_decl::CommandId::On | on_off_decl::CommandId::Toggle
  ))
  .with_events(with!());

const BOOLEAN_STATE_CLUSTER: Cluster<'static> = bool_state_cluster::CLUSTER;
const OCCUPANCY_CLUSTER: Cluster<'static> = occupancy_cluster::CLUSTER;
const WINDOW_COVERING_CLUSTER: Cluster<'static> = window_covering_cluster::CLUSTER;

static AGGREGATOR_DEVICE_TYPES: [MatterDeviceType; 1] = [DEV_TYPE_AGGREGATOR];
static AGGREGATOR_CLUSTERS: [Cluster<'static>; 1] = [DESC_CLUSTER];
static BRIDGED_ON_OFF_CLUSTERS: [Cluster<'static>; 3] = [DESC_CLUSTER, BRIDGED_INFO_CLUSTER, ON_OFF_CLUSTER];
static BRIDGED_CONTACT_CLUSTERS: [Cluster<'static>; 3] = [DESC_CLUSTER, BRIDGED_INFO_CLUSTER, BOOLEAN_STATE_CLUSTER];
static BRIDGED_MOTION_CLUSTERS: [Cluster<'static>; 3] = [DESC_CLUSTER, BRIDGED_INFO_CLUSTER, OCCUPANCY_CLUSTER];
static BRIDGED_COVER_CLUSTERS: [Cluster<'static>; 3] = [DESC_CLUSTER, BRIDGED_INFO_CLUSTER, WINDOW_COVERING_CLUSTER];
// Concrete device type first, `Bridged Node` second, matching upstream's bridge example.
static ON_OFF_LIGHT_DEVICE_TYPES: [MatterDeviceType; 2] = [DEV_TYPE_ON_OFF_LIGHT, DEV_TYPE_BRIDGED_NODE];
static ON_OFF_PLUG_DEVICE_TYPES: [MatterDeviceType; 2] = [DEV_TYPE_ON_OFF_PLUG_IN_UNIT, DEV_TYPE_BRIDGED_NODE];
static CONTACT_DEVICE_TYPES: [MatterDeviceType; 2] = [DEV_TYPE_CONTACT_SENSOR, DEV_TYPE_BRIDGED_NODE];
static MOTION_DEVICE_TYPES: [MatterDeviceType; 2] = [DEV_TYPE_OCCUPANCY_SENSOR, DEV_TYPE_BRIDGED_NODE];
static COVER_DEVICE_TYPES: [MatterDeviceType; 2] = [DEV_TYPE_WINDOW_COVERING, DEV_TYPE_BRIDGED_NODE];

/// Functional surface a slot exposes to controllers.
#[derive(Debug)]
pub enum SlotKind {
  /// On/off-capable export (light, switch, plug, outlet).
  OnOff { on: AtomicBool },
  /// Cover / garage: HA-scale position 0–100 (100 = open), target, and in-motion flag.
  Cover {
    position: AtomicU8,
    target: AtomicU8,
    moving: AtomicBool,
  },
  /// Contact sensor: Matter `StateValue` true = closed.
  Contact { closed: AtomicBool },
  /// Motion / occupancy sensor.
  Motion { occupied: AtomicBool },
}

/// One bridged endpoint: a catalog export plus its Matter-side state.
#[derive(Debug)]
pub struct ExportSlot {
  pub export_id: Uuid,
  /// Catalog `endpoint_id` + 1, so the aggregator keeps endpoint 1.
  pub matter_endpoint: EndptId,
  /// BDBI `NodeLabel` and the human-readable export name.
  pub name: String,
  pub kind: SlotKind,
  /// Catalog export backing this slot; drives the HA state mapping.
  export: Export,
  /// `UniqueID` attribute value: the export id without hyphens (32 chars).
  unique_id: String,
  /// Per-cluster data versions (rs-matter `Dataver` equivalents, but `Sync`).
  desc_dataver: AtomicU32,
  bridged_info_dataver: AtomicU32,
  functional_dataver: AtomicU32,
  /// Attributes that changed off-thread and still owe controllers a report.
  pending_reports: AtomicU32,
}

impl ExportSlot {
  /// Export name as configured in the catalog.
  pub fn name(&self) -> &str {
    &self.name
  }

  /// Current on/off value as controllers should see it.
  pub fn on(&self) -> bool {
    match &self.kind {
      SlotKind::OnOff { on } => on.load(Ordering::SeqCst),
      _ => false,
    }
  }

  /// Contact: Matter `StateValue` (true = closed).
  pub fn contact_closed(&self) -> bool {
    match &self.kind {
      SlotKind::Contact { closed } => closed.load(Ordering::SeqCst),
      _ => false,
    }
  }

  /// Motion: occupied bit.
  pub fn motion_occupied(&self) -> bool {
    match &self.kind {
      SlotKind::Motion { occupied } => occupied.load(Ordering::SeqCst),
      _ => false,
    }
  }

  /// Cover HA-scale position (0 = closed, 100 = open).
  pub fn cover_position(&self) -> u8 {
    match &self.kind {
      SlotKind::Cover { position, .. } => position.load(Ordering::SeqCst),
      _ => 0,
    }
  }

  /// Cover HA-scale target position.
  pub fn cover_target(&self) -> u8 {
    match &self.kind {
      SlotKind::Cover { target, .. } => target.load(Ordering::SeqCst),
      _ => 0,
    }
  }

  /// Whether the cover reports in-motion.
  pub fn cover_moving(&self) -> bool {
    match &self.kind {
      SlotKind::Cover { moving, .. } => moving.load(Ordering::SeqCst),
      _ => false,
    }
  }

  /// Store a new on/off value; returns `true` when it actually changed.
  fn store_on(&self, value: bool) -> bool {
    match &self.kind {
      SlotKind::OnOff { on } => on.swap(value, Ordering::SeqCst) != value,
      _ => false,
    }
  }

  /// Store contact closed flag; returns `true` when it changed.
  fn store_closed(&self, value: bool) -> bool {
    match &self.kind {
      SlotKind::Contact { closed } => closed.swap(value, Ordering::SeqCst) != value,
      _ => false,
    }
  }

  /// Store motion occupied flag; returns `true` when it changed.
  fn store_occupied(&self, value: bool) -> bool {
    match &self.kind {
      SlotKind::Motion { occupied } => occupied.swap(value, Ordering::SeqCst) != value,
      _ => false,
    }
  }

  /// Apply cover HA state; returns `true` when any field changed.
  fn apply_cover_ha(&self, ha: window_covering_cluster::CoverHaState) -> bool {
    let SlotKind::Cover {
      position,
      target,
      moving,
    } = &self.kind
    else {
      return false;
    };

    let mut changed = false;
    let cur_pos = position.load(Ordering::SeqCst);
    let cur_tgt = target.load(Ordering::SeqCst);
    let cur_mov = moving.load(Ordering::SeqCst);

    match ha.motion {
      CoverMotion::Opening => {
        if !cur_mov {
          moving.store(true, Ordering::SeqCst);
          changed = true;
        }
        // Keep commanded target; if HA started the motion, aim fully open.
        if cur_tgt <= cur_pos && cur_tgt != 100 {
          target.store(100, Ordering::SeqCst);
          changed = true;
        }
        if let Some(p) = ha.position
          && position.swap(p, Ordering::SeqCst) != p
        {
          changed = true;
        }
      }
      CoverMotion::Closing => {
        if !cur_mov {
          moving.store(true, Ordering::SeqCst);
          changed = true;
        }
        if cur_tgt >= cur_pos && cur_tgt != 0 {
          target.store(0, Ordering::SeqCst);
          changed = true;
        }
        if let Some(p) = ha.position
          && position.swap(p, Ordering::SeqCst) != p
        {
          changed = true;
        }
      }
      CoverMotion::Stopped => {
        if cur_mov {
          moving.store(false, Ordering::SeqCst);
          changed = true;
        }
        if let Some(p) = ha.position {
          if position.swap(p, Ordering::SeqCst) != p {
            changed = true;
          }
          if target.swap(p, Ordering::SeqCst) != p {
            changed = true;
          }
        }
      }
    }
    changed
  }

  /// Controller open: target fully open, mark moving.
  fn cover_command_open(&self) {
    if let SlotKind::Cover { target, moving, .. } = &self.kind {
      target.store(100, Ordering::SeqCst);
      moving.store(true, Ordering::SeqCst);
    }
  }

  /// Controller close: target fully closed, mark moving.
  fn cover_command_close(&self) {
    if let SlotKind::Cover { target, moving, .. } = &self.kind {
      target.store(0, Ordering::SeqCst);
      moving.store(true, Ordering::SeqCst);
    }
  }

  /// Controller stop: freeze target at current position.
  fn cover_command_stop(&self) {
    if let SlotKind::Cover {
      position,
      target,
      moving,
    } = &self.kind
    {
      let p = position.load(Ordering::SeqCst);
      target.store(p, Ordering::SeqCst);
      moving.store(false, Ordering::SeqCst);
    }
  }

  /// Controller go-to-position (HA scale 0–100).
  fn cover_command_position(&self, ha_position: u8) {
    if let SlotKind::Cover { target, moving, .. } = &self.kind {
      target.store(ha_position.min(100), Ordering::SeqCst);
      moving.store(true, Ordering::SeqCst);
    }
  }

  fn node_label(&self) -> Utf8Str<'_> {
    clamp_utf8(self.name(), NODE_LABEL_MAX_BYTES)
  }

  fn device_types(&self) -> &'static [MatterDeviceType] {
    match self.export.type_ {
      DeviceType::Light => &ON_OFF_LIGHT_DEVICE_TYPES,
      DeviceType::OnOffSwitch | DeviceType::OnOffPlug | DeviceType::Outlet => &ON_OFF_PLUG_DEVICE_TYPES,
      DeviceType::Contact => &CONTACT_DEVICE_TYPES,
      DeviceType::Motion => &MOTION_DEVICE_TYPES,
      DeviceType::Cover | DeviceType::Garage => &COVER_DEVICE_TYPES,
    }
  }

  fn clusters(&self) -> &'static [Cluster<'static>] {
    match &self.kind {
      SlotKind::OnOff { .. } => &BRIDGED_ON_OFF_CLUSTERS,
      SlotKind::Contact { .. } => &BRIDGED_CONTACT_CLUSTERS,
      SlotKind::Motion { .. } => &BRIDGED_MOTION_CLUSTERS,
      SlotKind::Cover { .. } => &BRIDGED_COVER_CLUSTERS,
    }
  }

  /// Functional cluster id for this slot (for dataver / report routing).
  fn functional_cluster_id(&self) -> ClusterId {
    match &self.kind {
      SlotKind::OnOff { .. } => ON_OFF_CLUSTER.id,
      SlotKind::Contact { .. } => BOOLEAN_STATE_CLUSTER.id,
      SlotKind::Motion { .. } => OCCUPANCY_CLUSTER.id,
      SlotKind::Cover { .. } => WINDOW_COVERING_CLUSTER.id,
    }
  }

  fn endpoint(&self) -> Endpoint<'static> {
    Endpoint::new(self.matter_endpoint, self.device_types(), self.clusters())
  }

  /// Identity of the slot as controllers see it; a change means the exposed
  /// node shape moved and the configuration version has to be bumped.
  /// The concrete device type comes first in `device_types()`.
  fn surface(&self) -> (EndptId, Uuid, u16) {
    (self.matter_endpoint, self.export_id, self.device_types()[0].dtype)
  }

  fn request_report(&self, bits: u32) {
    self.pending_reports.fetch_or(bits, Ordering::SeqCst);
  }

  fn mark_functional_changed(&self) {
    self.functional_dataver.fetch_add(1, Ordering::SeqCst);
    self.request_report(REPORT_FUNCTIONAL);
  }
}

/// Immutable snapshot of the bridged surface, swapped wholesale on rebuild.
struct PlaneState {
  /// Bridged slots, ordered by `matter_endpoint` ascending.
  slots: Vec<ExportSlot>,
  /// Root + aggregator + one endpoint per slot, ids strictly increasing.
  endpoints: Vec<Endpoint<'static>>,
  /// Every export id the catalog knows, bridged or not.
  known: BTreeSet<Uuid>,
}

impl PlaneState {
  fn new(slots: Vec<ExportSlot>, known: BTreeSet<Uuid>) -> Self {
    let mut endpoints = Vec::with_capacity(slots.len() + 2);
    endpoints.push(EthMatterStack::<0, ()>::root_endpoint());
    endpoints.push(Endpoint::new(
      AGGREGATOR_ENDPOINT_ID,
      &AGGREGATOR_DEVICE_TYPES,
      &AGGREGATOR_CLUSTERS,
    ));
    endpoints.extend(slots.iter().map(ExportSlot::endpoint));
    Self {
      slots,
      endpoints,
      known,
    }
  }

  fn slot_at(&self, matter_endpoint: EndptId) -> Option<&ExportSlot> {
    self
      .slots
      .binary_search_by_key(&matter_endpoint, |slot| slot.matter_endpoint)
      .ok()
      .map(|idx| &self.slots[idx])
  }

  fn slot_for(&self, export_id: Uuid) -> Option<&ExportSlot> {
    self.slots.iter().find(|slot| slot.export_id == export_id)
  }
}

/// Slot table plus the wake-up plumbing shared with the Matter stack thread.
pub struct ExportPlane {
  state: RwLock<Arc<PlaneState>>,
  /// Wakes `run()`: emit subscription reports, bump the configuration version,
  /// or process a pairing-window request.
  changed: Notify,
  commands: Mutex<VecDeque<CommandRequest>>,
  /// Generation of the latest surface that still owes a `ConfigurationVersion` bump.
  /// Advanced by `set_exports` on every real surface change; never goes backwards.
  config_requested: AtomicU64,
  /// Last generation the stack thread (or a test stand-in) successfully applied.
  /// A bump is pending while `config_requested != config_applied`. Acknowledge only
  /// the generation observed before the bump so a concurrent `set_exports` cannot
  /// clear a newer request (the AtomicBool load/store race).
  config_applied: AtomicU64,
  aggregator_dataver: AtomicU32,
  /// Epoch seconds when the basic pairing window closes; 0 = closed.
  /// Tracked by us: rs-matter does not expose a read-only window probe.
  window_deadline: AtomicU64,
  /// Last sampled commissioned fabric count (`with_state` on the stack thread).
  fabric_count: AtomicU8,
  /// Pending open/close from the IPC plane; drained only on the stack thread.
  window_request: Mutex<Option<WindowRequest>>,
}

impl Default for ExportPlane {
  fn default() -> Self {
    Self::new()
  }
}

impl ExportPlane {
  pub fn new() -> Self {
    Self {
      state: RwLock::new(Arc::new(PlaneState::new(Vec::new(), BTreeSet::new()))),
      changed: Notify::new(),
      commands: Mutex::new(VecDeque::new()),
      config_requested: AtomicU64::new(0),
      config_applied: AtomicU64::new(0),
      aggregator_dataver: AtomicU32::new(rand::random()),
      window_deadline: AtomicU64::new(0),
      fabric_count: AtomicU8::new(0),
      window_request: Mutex::new(None),
    }
  }

  /// Whether the basic commissioning window is currently open (deadline-based).
  pub fn pairing_open(&self) -> bool {
    let deadline = self.window_deadline.load(Ordering::SeqCst);
    deadline != 0 && epoch_secs() < deadline
  }

  /// Last fabric count sampled on the Matter stack thread.
  pub fn commissioned_fabrics(&self) -> u8 {
    self.fabric_count.load(Ordering::SeqCst)
  }

  /// Seed window + fabric state after stack `startup` (must run on the stack thread
  /// context that already opened a 900s window when fabric count is zero).
  pub fn note_startup_commissioning_state(&self, fabric_count: u8) {
    self.fabric_count.store(fabric_count, Ordering::SeqCst);
    if fabric_count == 0 {
      let deadline = epoch_secs().saturating_add(STARTUP_PAIRING_TIMEOUT_SECS);
      self.window_deadline.store(deadline, Ordering::SeqCst);
      tracing::info!(
        timeout_secs = STARTUP_PAIRING_TIMEOUT_SECS,
        "pairing window open at startup (no fabrics)"
      );
    } else {
      self.window_deadline.store(0, Ordering::SeqCst);
    }
  }

  /// Record the current fabric count as the sampling baseline without clearing the
  /// window. Used before storing a fresh open deadline so a later `apply_fabric_sample`
  /// cannot treat the open as stale (`count > prev` with an outdated `prev`).
  fn establish_fabric_baseline(&self, count: u8) {
    self.fabric_count.store(count, Ordering::SeqCst);
  }

  /// Apply a fabric-count sample: update the baseline, clear the window when a new
  /// fabric appears, and lazily expire an overdue deadline.
  fn apply_fabric_sample(&self, count: u8) {
    let prev = self.fabric_count.swap(count, Ordering::SeqCst);
    if count > prev {
      // Commissioning completed (or another fabric was added): window is done.
      self.window_deadline.store(0, Ordering::SeqCst);
      tracing::info!(fabrics = count, "fabric count increased; pairing window closed");
    }
    // Lazy expiry: pairing_open() already compares now < deadline, but clear the
    // atomic so status stays honest without a clock on every poll.
    let deadline = self.window_deadline.load(Ordering::SeqCst);
    if deadline != 0 && epoch_secs() >= deadline {
      self.window_deadline.store(0, Ordering::SeqCst);
      tracing::info!("pairing window expired");
    }
  }

  /// Mark the bridge-tracked window closed (deadline 0).
  fn mark_window_closed(&self) {
    self.window_deadline.store(0, Ordering::SeqCst);
  }

  /// Mark a successfully opened window: re-establish fabric baseline, then store the
  /// new deadline. Ordering matters so a fabric sample in the same loop iteration
  /// cannot wipe this open via a stale `prev`.
  fn mark_window_opened(&self, timeout_secs: u16, fabric_count: u8) {
    self.establish_fabric_baseline(fabric_count);
    let deadline = epoch_secs().saturating_add(u64::from(timeout_secs));
    self.window_deadline.store(deadline, Ordering::SeqCst);
  }

  /// Ask the stack thread to open a basic commissioning window.
  pub fn request_open_window(&self, timeout_secs: u16) -> anyhow::Result<()> {
    let timeout = clamp_pairing_timeout(timeout_secs);
    self.post_window_request(WindowOp::Open(timeout))
  }

  /// Ask the stack thread to close any window this bridge opened.
  ///
  /// Idempotent when no bridge-tracked window is open (does not revoke
  /// controller-opened ECM windows we never tracked).
  pub fn request_close_window(&self) -> anyhow::Result<()> {
    self.post_window_request(WindowOp::Close)
  }

  fn post_window_request(&self, op: WindowOp) -> anyhow::Result<()> {
    let (tx, rx) = std::sync::mpsc::channel();
    {
      let mut slot = self.window_request.lock();
      if slot.is_some() {
        anyhow::bail!("pairing window request already in flight");
      }
      *slot = Some(WindowRequest { op, reply: tx });
    }
    self.changed.notify_one();
    match rx.recv_timeout(Duration::from_secs(10)) {
      Ok(Ok(())) => Ok(()),
      Ok(Err(msg)) => anyhow::bail!(msg),
      Err(_) => {
        // Stack never drained the request (died or never entered run). Clear so
        // a later open/close is not stuck behind a dead mailbox entry.
        let mut slot = self.window_request.lock();
        if slot.is_some() {
          *slot = None;
        }
        anyhow::bail!("pairing window request timed out waiting for the Matter stack")
      }
    }
  }

  /// Rebuild the slot table from the catalog, preserving per-export state.
  ///
  /// Slot identity is the `export_id`: functional state and data versions carry
  /// over so controllers never see an endpoint go backwards.
  pub fn set_exports(&self, exports: &[Export]) {
    let mut bridged: Vec<&Export> = exports.iter().filter(|e| is_matter_bridged_export(e)).collect();
    bridged.sort_by_key(|e| (e.endpoint_id.unwrap_or(EndptId::MAX), e.export_id));
    let known: BTreeSet<Uuid> = exports.iter().map(|e| e.export_id).collect();

    // The write guard spans the whole read-copy-swap: slot state is carried over
    // by value, so a mutation landing between the copy and the swap would be lost.
    let (surface_changed, work_pending) = {
      let mut guard = self.state.write();
      let previous = Arc::clone(&guard);

      let mut slots: Vec<ExportSlot> = Vec::with_capacity(bridged.len());
      let mut taken: BTreeSet<EndptId> = BTreeSet::new();
      for export in bridged {
        let Some(matter_endpoint) = export.endpoint_id.and_then(|id| id.checked_add(1)) else {
          tracing::warn!(export_id = %export.export_id, endpoint_id = ?export.endpoint_id, "export has no usable endpoint id; not bridged");
          continue;
        };
        if matter_endpoint <= AGGREGATOR_ENDPOINT_ID || !taken.insert(matter_endpoint) {
          tracing::warn!(export_id = %export.export_id, matter_endpoint, "endpoint conflicts with the bridge layout; not bridged");
          continue;
        }
        slots.push(ExportSlot::rebuild(
          export,
          matter_endpoint,
          previous.slot_for(export.export_id),
        ));
      }

      let surface_changed = previous.slots.len() != slots.len()
        || previous
          .slots
          .iter()
          .zip(slots.iter())
          .any(|(old, new)| old.surface() != new.surface());

      let next = Arc::new(PlaneState::new(slots, known));
      let work_pending = next.slots.iter().any(|s| s.pending_reports.load(Ordering::SeqCst) != 0);
      *guard = next;
      (surface_changed, work_pending)
    };

    if surface_changed {
      self.aggregator_dataver.fetch_add(1, Ordering::SeqCst);
      // Bump the generation rather than flipping a bool: a concurrent run-loop
      // clear of an older generation cannot erase this request.
      self.config_requested.fetch_add(1, Ordering::SeqCst);
    }
    if surface_changed || work_pending {
      self.changed.notify_one();
    }
  }

  /// Apply HA state to a slot. Never enqueues a controller command: HA is the
  /// source of truth here, echoing it back would loop.
  pub fn apply_state(&self, export_id: Uuid, states: &[HaStateValue]) -> u32 {
    let (applied, changed) = self.with_state(|state| {
      let Some(slot) = state.slot_for(export_id) else {
        if !state.known.contains(&export_id) {
          tracing::warn!(%export_id, "apply_state for unknown export");
          return (0, false);
        }
        // Known but disabled / unassigned endpoint.
        return (states.len() as u32, false);
      };

      let changed = match &slot.kind {
        SlotKind::OnOff { .. } => {
          if let Some(on) = on_off_from_states(&slot.export, states)
            && slot.store_on(on)
          {
            slot.mark_functional_changed();
            tracing::debug!(%export_id, on, endpoint = slot.matter_endpoint, "HA state applied to Matter OnOff");
            true
          } else {
            false
          }
        }
        SlotKind::Contact { .. } => {
          if let Some(on) = primary_ha_on(&slot.export, states) {
            let closed = bool_state_cluster::state_value_from_ha_on(on);
            if slot.store_closed(closed) {
              slot.mark_functional_changed();
              tracing::debug!(%export_id, closed, endpoint = slot.matter_endpoint, "HA state applied to BooleanState");
              true
            } else {
              false
            }
          } else {
            false
          }
        }
        SlotKind::Motion { .. } => {
          if let Some(on) = primary_ha_on(&slot.export, states) {
            if slot.store_occupied(on) {
              slot.mark_functional_changed();
              tracing::debug!(%export_id, occupied = on, endpoint = slot.matter_endpoint, "HA state applied to Occupancy");
              true
            } else {
              false
            }
          } else {
            false
          }
        }
        SlotKind::Cover { .. } => {
          if let Some(ha) = primary_cover_state(&slot.export, states)
            && slot.apply_cover_ha(ha)
          {
            slot.mark_functional_changed();
            tracing::debug!(
              %export_id,
              position = slot.cover_position(),
              target = slot.cover_target(),
              moving = slot.cover_moving(),
              endpoint = slot.matter_endpoint,
              "HA state applied to WindowCovering"
            );
            true
          } else {
            false
          }
        }
      };
      (states.len() as u32, changed)
    });

    if changed {
      self.changed.notify_one();
    }
    applied
  }

  /// Apply a controller on/off write and queue the matching HA command.
  ///
  /// Returns `false` when no slot serves `matter_endpoint`.
  pub fn apply_controller_on_off(&self, matter_endpoint: EndptId, on: bool) -> bool {
    self.with_state(|state| {
      let Some(slot) = state.slot_at(matter_endpoint) else {
        return false;
      };
      if !matches!(slot.kind, SlotKind::OnOff { .. }) {
        return false;
      }
      if slot.store_on(on) {
        slot.functional_dataver.fetch_add(1, Ordering::SeqCst);
      }
      self.commands.lock().push_back(on_off_command(slot.export_id, on));
      tracing::info!(
        export_id = %slot.export_id,
        on,
        endpoint = slot.matter_endpoint,
        "Matter controller OnOff → command queue"
      );
      true
    })
  }

  /// Apply a controller Window Covering command and queue the matching HA command.
  ///
  /// Returns `false` when no cover slot serves `matter_endpoint`.
  pub fn apply_controller_cover(&self, matter_endpoint: EndptId, kind: CommandKind, ha_position: Option<u8>) -> bool {
    self.with_state(|state| {
      let Some(slot) = state.slot_at(matter_endpoint) else {
        return false;
      };
      if !matches!(slot.kind, SlotKind::Cover { .. }) {
        return false;
      }
      match kind {
        CommandKind::CoverOpen => slot.cover_command_open(),
        CommandKind::CoverClose => slot.cover_command_close(),
        CommandKind::CoverStop => slot.cover_command_stop(),
        CommandKind::CoverPosition => {
          let p = ha_position.unwrap_or(slot.cover_target());
          slot.cover_command_position(p);
        }
        _ => return false,
      }
      slot.functional_dataver.fetch_add(1, Ordering::SeqCst);
      self.commands.lock().push_back(CommandRequest {
        export_id: slot.export_id,
        kind,
        on: None,
        level: None,
        position: match kind {
          CommandKind::CoverPosition => Some(ha_position.unwrap_or(slot.cover_target()).min(100)),
          _ => None,
        },
      });
      tracing::info!(
        export_id = %slot.export_id,
        ?kind,
        position = ?ha_position,
        endpoint = slot.matter_endpoint,
        "Matter controller WindowCovering → command queue"
      );
      true
    })
  }

  /// Drain the queue of commands raised by Matter controllers.
  pub fn take_commands(&self) -> Vec<CommandRequest> {
    self.commands.lock().drain(..).collect()
  }

  /// Drain the attribute changes that still owe controllers a report.
  pub fn drain_reports(&self) -> Vec<(EndptId, ClusterId, AttrId)> {
    self.with_state(|state| {
      let mut out = Vec::new();
      for slot in &state.slots {
        let bits = slot.pending_reports.swap(0, Ordering::SeqCst);
        if bits & REPORT_FUNCTIONAL != 0 {
          match &slot.kind {
            SlotKind::OnOff { .. } => {
              out.push((
                slot.matter_endpoint,
                ON_OFF_CLUSTER.id,
                on_off_decl::AttributeId::OnOff as AttrId,
              ));
            }
            SlotKind::Contact { .. } => {
              out.push((
                slot.matter_endpoint,
                BOOLEAN_STATE_CLUSTER.id,
                bool_state_cluster::AttributeId::StateValue as AttrId,
              ));
            }
            SlotKind::Motion { .. } => {
              out.push((
                slot.matter_endpoint,
                OCCUPANCY_CLUSTER.id,
                occupancy_cluster::AttributeId::Occupancy as AttrId,
              ));
            }
            SlotKind::Cover { .. } => {
              let ep = slot.matter_endpoint;
              let cid = WINDOW_COVERING_CLUSTER.id;
              out.push((
                ep,
                cid,
                window_covering_cluster::AttributeId::CurrentPositionLiftPercent100ths as AttrId,
              ));
              out.push((
                ep,
                cid,
                window_covering_cluster::AttributeId::TargetPositionLiftPercent100ths as AttrId,
              ));
              out.push((
                ep,
                cid,
                window_covering_cluster::AttributeId::OperationalStatus as AttrId,
              ));
            }
          }
        }
        if bits & REPORT_NODE_LABEL != 0 {
          out.push((
            slot.matter_endpoint,
            BRIDGED_INFO_CLUSTER.id,
            bridged_info::AttributeId::NodeLabel as AttrId,
          ));
        }
        if bits & REPORT_DESCRIPTOR != 0 {
          out.push((
            slot.matter_endpoint,
            DESC_CLUSTER.id,
            desc::AttributeId::DeviceTypeList as AttrId,
          ));
        }
      }
      out
    })
  }

  /// Matter endpoint ids currently exposed, ascending.
  pub fn endpoint_ids(&self) -> Vec<EndptId> {
    self.snapshot().slots.iter().map(|s| s.matter_endpoint).collect()
  }

  /// Current snapshot; cheap (one `Arc` clone), and the lock is released before
  /// callers touch the data.
  ///
  /// Use this on paths that hand control back to `rs-matter`, which re-enters
  /// [`Metadata::access`] and [`AsyncHandler::bump_dataver`] while serving a
  /// read. Holding a `parking_lot` read guard across those calls would nest
  /// read locks and deadlock as soon as a rebuild queued a writer.
  fn snapshot(&self) -> Arc<PlaneState> {
    self.state.read().clone()
  }

  /// Run `f` with the slot table pinned.
  ///
  /// Every mutation of slot state goes through here, and `set_exports` holds
  /// the write guard across the whole rebuild, so a rebuild can never copy
  /// slot state out from under a concurrent write and lose it. `f` must not
  /// call back into `rs-matter` (see [`Self::snapshot`]).
  fn with_state<R>(&self, f: impl FnOnce(&PlaneState) -> R) -> R {
    f(&self.state.read())
  }

  fn aggregator_desc(&self) -> desc::DescHandler<'static> {
    desc::DescHandler::new_aggregator(Dataver::new(self.aggregator_dataver.load(Ordering::SeqCst)))
  }
}

impl ExportSlot {
  fn rebuild(export: &Export, matter_endpoint: EndptId, previous: Option<&ExportSlot>) -> Self {
    let kind = Self::rebuild_kind(export, previous);
    let slot = Self {
      export_id: export.export_id,
      matter_endpoint,
      name: export.name.clone(),
      kind,
      unique_id: export.export_id.simple().to_string(),
      export: export.clone(),
      desc_dataver: AtomicU32::new(previous.map_or_else(rand::random, |p| p.desc_dataver.load(Ordering::SeqCst))),
      bridged_info_dataver: AtomicU32::new(
        previous.map_or_else(rand::random, |p| p.bridged_info_dataver.load(Ordering::SeqCst)),
      ),
      functional_dataver: AtomicU32::new(
        previous.map_or_else(rand::random, |p| p.functional_dataver.load(Ordering::SeqCst)),
      ),
      pending_reports: AtomicU32::new(previous.map_or(0, |p| p.pending_reports.load(Ordering::SeqCst))),
    };
    if previous.is_some_and(|p| p.node_label() != slot.node_label()) {
      slot.bridged_info_dataver.fetch_add(1, Ordering::SeqCst);
      slot.request_report(REPORT_NODE_LABEL);
    }
    if previous.is_some_and(|p| p.surface() != slot.surface()) {
      // DeviceTypeList (and the rest of Descriptor) moved on this bridged endpoint.
      // Bump the data version and schedule a subscription notify so controllers that
      // watch the endpoint Descriptor — not only root/aggregator PartsList — refresh.
      slot.desc_dataver.fetch_add(1, Ordering::SeqCst);
      slot.request_report(REPORT_DESCRIPTOR);
    }
    slot
  }

  fn rebuild_kind(export: &Export, previous: Option<&ExportSlot>) -> SlotKind {
    match export.type_ {
      DeviceType::Light | DeviceType::OnOffSwitch | DeviceType::OnOffPlug | DeviceType::Outlet => {
        let on = previous.map(ExportSlot::on).unwrap_or(false);
        SlotKind::OnOff {
          on: AtomicBool::new(on),
        }
      }
      DeviceType::Contact => {
        let closed = previous.map(ExportSlot::contact_closed).unwrap_or(true);
        SlotKind::Contact {
          closed: AtomicBool::new(closed),
        }
      }
      DeviceType::Motion => {
        let occupied = previous.map(ExportSlot::motion_occupied).unwrap_or(false);
        SlotKind::Motion {
          occupied: AtomicBool::new(occupied),
        }
      }
      DeviceType::Cover | DeviceType::Garage => {
        // Default fully open when no prior state (HA 100 = open).
        let (position, target, moving) = match previous.map(|p| &p.kind) {
          Some(SlotKind::Cover {
            position,
            target,
            moving,
          }) => (
            position.load(Ordering::SeqCst),
            target.load(Ordering::SeqCst),
            moving.load(Ordering::SeqCst),
          ),
          _ => (100, 100, false),
        };
        SlotKind::Cover {
          position: AtomicU8::new(position),
          target: AtomicU8::new(target),
          moving: AtomicBool::new(moving),
        }
      }
    }
  }
}

/// Primary-entity HA on/off (contact / motion). Linked entities never drive it.
fn primary_ha_on(export: &Export, states: &[HaStateValue]) -> Option<bool> {
  states
    .iter()
    .find(|st| st.entity_id == export.primary_entity_id)
    .and_then(|st| ha_state_is_on(&st.state))
}

/// Primary-entity cover state.
fn primary_cover_state(export: &Export, states: &[HaStateValue]) -> Option<window_covering_cluster::CoverHaState> {
  states
    .iter()
    .find(|st| st.entity_id == export.primary_entity_id)
    .map(ha_cover_from_state)
}

/// Clamp to at most `max_bytes`, never splitting a UTF-8 code point.
fn clamp_utf8(value: &str, max_bytes: usize) -> &str {
  if value.len() <= max_bytes {
    return value;
  }
  let mut end = max_bytes;
  while !value.is_char_boundary(end) {
    end -= 1;
  }
  &value[..end]
}

/// Matches everything the aggregator and the bridged endpoints own, i.e. every
/// endpoint the root chain of `rs-matter-stack` does not already serve.
pub struct BridgedEndpointMatcher;

impl Matcher for BridgedEndpointMatcher {
  fn matches(&self, ctx: impl MatchContext) -> bool {
    ctx.endpt().is_none_or(|endpoint| endpoint >= AGGREGATOR_ENDPOINT_ID)
  }
}

impl Metadata for ExportPlane {
  fn access<F, R>(&self, f: F) -> R
  where
    F: FnOnce(&Node<'_>) -> R,
  {
    let state = self.snapshot();
    f(&Node {
      endpoints: &state.endpoints,
    })
  }
}

impl AsyncHandler for ExportPlane {
  fn read_awaits(&self, _ctx: impl ReadContext) -> bool {
    false
  }

  fn write_awaits(&self, _ctx: impl WriteContext) -> bool {
    false
  }

  fn invoke_awaits(&self, _ctx: impl InvokeContext) -> bool {
    false
  }

  async fn write(&self, ctx: impl WriteContext) -> Result<(), Error> {
    let (endpoint, cluster) = (ctx.attr().endpoint_id, ctx.attr().cluster_id);

    let state = self.snapshot();
    let Some(slot) = state.slot_at(endpoint) else {
      return Err(ErrorCode::EndpointNotFound.into());
    };

    // Window Covering `Mode` is the only writable functional attribute we
    // advertise. Bridged `NodeLabel` stays catalog-owned → AttributeNotFound.
    if cluster == WINDOW_COVERING_CLUSTER.id && matches!(slot.kind, SlotKind::Cover { .. }) {
      return Handler::write(
        &window_covering_cluster::HandlerAdaptor(SlotWindowCoveringHandler { plane: self, slot }),
        ctx,
      );
    }

    Err(ErrorCode::AttributeNotFound.into())
  }

  async fn read(&self, ctx: impl ReadContext, reply: impl ReadReply) -> Result<(), Error> {
    let (endpoint, cluster) = (ctx.attr().endpoint_id, ctx.attr().cluster_id);

    if endpoint == AGGREGATOR_ENDPOINT_ID {
      if cluster != DESC_CLUSTER.id {
        return Err(ErrorCode::ClusterNotFound.into());
      }
      return Handler::read(&self.aggregator_desc().adapt(), ctx, reply);
    }

    let state = self.snapshot();
    let Some(slot) = state.slot_at(endpoint) else {
      return Err(ErrorCode::EndpointNotFound.into());
    };

    match cluster {
      id if id == DESC_CLUSTER.id => {
        let dataver = Dataver::new(slot.desc_dataver.load(Ordering::SeqCst));
        Handler::read(&desc::DescHandler::new(dataver).adapt(), ctx, reply)
      }
      id if id == BRIDGED_INFO_CLUSTER.id => {
        Handler::read(&bridged_info::HandlerAdaptor(BridgedInfoHandler { slot }), ctx, reply)
      }
      id if id == ON_OFF_CLUSTER.id && matches!(slot.kind, SlotKind::OnOff { .. }) => Handler::read(
        &on_off_decl::HandlerAdaptor(SlotOnOffHandler { plane: self, slot }),
        ctx,
        reply,
      ),
      id if id == BOOLEAN_STATE_CLUSTER.id && matches!(slot.kind, SlotKind::Contact { .. }) => Handler::read(
        &bool_state_cluster::HandlerAdaptor(SlotBooleanStateHandler { slot }),
        ctx,
        reply,
      ),
      id if id == OCCUPANCY_CLUSTER.id && matches!(slot.kind, SlotKind::Motion { .. }) => Handler::read(
        &occupancy_cluster::HandlerAdaptor(SlotOccupancyHandler { slot }),
        ctx,
        reply,
      ),
      id if id == WINDOW_COVERING_CLUSTER.id && matches!(slot.kind, SlotKind::Cover { .. }) => Handler::read(
        &window_covering_cluster::HandlerAdaptor(SlotWindowCoveringHandler { plane: self, slot }),
        ctx,
        reply,
      ),
      _ => Err(ErrorCode::ClusterNotFound.into()),
    }
  }

  async fn invoke(&self, ctx: impl InvokeContext, reply: impl InvokeReply) -> Result<(), Error> {
    let (endpoint, cluster) = (ctx.cmd().endpoint_id, ctx.cmd().cluster_id);

    let state = self.snapshot();
    let Some(slot) = state.slot_at(endpoint) else {
      return Err(ErrorCode::EndpointNotFound.into());
    };

    if cluster == ON_OFF_CLUSTER.id && matches!(slot.kind, SlotKind::OnOff { .. }) {
      return Handler::invoke(
        &on_off_decl::HandlerAdaptor(SlotOnOffHandler { plane: self, slot }),
        ctx,
        reply,
      );
    }
    if cluster == WINDOW_COVERING_CLUSTER.id && matches!(slot.kind, SlotKind::Cover { .. }) {
      return Handler::invoke(
        &window_covering_cluster::HandlerAdaptor(SlotWindowCoveringHandler { plane: self, slot }),
        ctx,
        reply,
      );
    }
    Err(ErrorCode::CommandNotFound.into())
  }

  /// Every slot has to be visited: one notification can match several handlers,
  /// and stopping at the first match would leave stale data versions behind.
  fn bump_dataver(&self, ctx: impl MatchContext) {
    let endpoint = ctx.endpt();
    let cluster = ctx.cluster();
    let hits_endpoint = |id: EndptId| endpoint.is_none_or(|e| e == id);
    let hits_cluster = |id: ClusterId| cluster.is_none_or(|c| c == id);

    if hits_endpoint(AGGREGATOR_ENDPOINT_ID) && hits_cluster(DESC_CLUSTER.id) {
      self.aggregator_dataver.fetch_add(1, Ordering::SeqCst);
    }

    self.with_state(|state| {
      for slot in &state.slots {
        if !hits_endpoint(slot.matter_endpoint) {
          continue;
        }
        if hits_cluster(DESC_CLUSTER.id) {
          slot.desc_dataver.fetch_add(1, Ordering::SeqCst);
        }
        if hits_cluster(BRIDGED_INFO_CLUSTER.id) {
          slot.bridged_info_dataver.fetch_add(1, Ordering::SeqCst);
        }
        if hits_cluster(slot.functional_cluster_id()) {
          slot.functional_dataver.fetch_add(1, Ordering::SeqCst);
        }
      }
    });
  }

  /// Turns off-thread changes into data-model notifications. Runs forever on the
  /// stack thread, which is the only place `Matter` may be touched.
  async fn run(&self, ctx: impl HandlerContext) -> Result<(), Error> {
    loop {
      // Wake on export changes / window requests, or periodically to sample fabrics
      // and expire the pairing-window deadline.
      {
        let wake = async {
          self.changed.notified().await;
        };
        let tick = async {
          async_io::Timer::after(FABRIC_SAMPLE_INTERVAL).await;
        };
        futures_lite::future::race(wake, tick).await;
      }

      // Sample fabrics first so a pending fabric increase updates the baseline
      // (and closes any prior window) before we process a fresh open request.
      // Open then re-baselines immediately before storing its deadline.
      self.sample_fabrics(&ctx);
      self.drain_window_request(&ctx);

      // Drain every outstanding config generation. Acknowledge only the generation
      // observed before the bump: if `set_exports` advanced the counter while we
      // were inside `bump_configuration_version`, the newer request stays pending
      // and this loop continues without waiting for another notify.
      loop {
        let requested = self.config_requested.load(Ordering::SeqCst);
        let applied = self.config_applied.load(Ordering::SeqCst);
        if requested == applied {
          break;
        }
        match ctx.matter().bump_configuration_version(ctx.kv(), &ctx) {
          Ok(version) => {
            // Compare-and-clear: only advances `config_applied` up to `requested`.
            // A concurrent fetch_add leaves requested > applied, so we loop again.
            self.acknowledge_config_generation(requested);
            tracing::info!(
              configuration_version = version,
              endpoints = ?self.endpoint_ids(),
              "bridged endpoint set changed"
            );
          }
          Err(err) => {
            // Leave the generation pending so a later wake retries the persist.
            tracing::warn!(error = ?err, "configuration version bump failed; retrying on the next change");
            break;
          }
        }
        // PartsList moved on the root endpoint and on the aggregator. Bridged
        // endpoint Descriptor changes are reported via `drain_reports` below.
        ctx.notify_cluster_changed(ROOT_ENDPOINT_ID, DESC_CLUSTER.id);
        ctx.notify_cluster_changed(AGGREGATOR_ENDPOINT_ID, DESC_CLUSTER.id);
      }

      for (endpoint, cluster, attr) in self.drain_reports() {
        ctx.notify_attr_changed(endpoint, cluster, attr);
      }
    }
  }
}

impl ExportPlane {
  /// Process one pending open/close (if any). Must run on the Matter stack thread.
  fn drain_window_request(&self, ctx: &impl HandlerContext) {
    let Some(req) = self.window_request.lock().take() else {
      return;
    };
    let result = self.execute_window_op(
      req.op,
      || Self::read_fabric_count(ctx),
      || ctx.matter().close_comm_window(ctx).map_err(|err| format!("{err:?}")),
      |timeout_secs| {
        ctx
          .matter()
          .open_basic_comm_window(timeout_secs, ctx.crypto(), ctx)
          .map_err(|err| format!("{err:?}"))
      },
    );
    let _ = req.reply.send(result);
  }

  /// Pairing-window open/close decisions with injectable stack ops.
  ///
  /// Production binds the callbacks to the live Matter stack; unit tests inject
  /// counters and controlled success/failure without a real stack.
  fn execute_window_op(
    &self,
    op: WindowOp,
    mut fabric_count: impl FnMut() -> u8,
    mut close_comm_window: impl FnMut() -> Result<bool, String>,
    mut open_basic_comm_window: impl FnMut(u16) -> Result<(), String>,
  ) -> Result<(), String> {
    match op {
      WindowOp::Open(timeout_secs) => {
        // Already open: do not close/reopen (avoids thrashing the stack window
        // and mDNS). Optionally extend the tracked deadline without touching
        // the stack — rs-matter rejects open while a window is already open.
        if self.pairing_open() {
          let count = fabric_count();
          self.establish_fabric_baseline(count);
          let now = epoch_secs();
          let proposed = now.saturating_add(u64::from(timeout_secs));
          let current = self.window_deadline.load(Ordering::SeqCst);
          if proposed > current {
            self.window_deadline.store(proposed, Ordering::SeqCst);
            tracing::info!(timeout_secs, "pairing window already open; extended tracked deadline");
          } else {
            tracing::info!(timeout_secs, "pairing window already open; skipping close/reopen");
          }
          return Ok(());
        }

        // Baseline fabrics before open so a same-iteration sample (or a count
        // that already rose while we still held a stale prev) cannot wipe the
        // new deadline via `count > prev`.
        let count = fabric_count();
        self.establish_fabric_baseline(count);

        // Window is closed in our tracking, but the stack may still hold one
        // (e.g. startup window we already expired). Close first so open succeeds.
        // On successful close, clear our deadline immediately: if the subsequent
        // open fails we must stay closed (no stale pairing_open).
        match close_comm_window() {
          Ok(_) => self.mark_window_closed(),
          Err(err) => {
            tracing::warn!(error = %err, "pre-open close_comm_window failed; still attempting open");
          }
        }

        match open_basic_comm_window(timeout_secs) {
          Ok(()) => {
            let count = fabric_count();
            self.mark_window_opened(timeout_secs, count);
            tracing::info!(timeout_secs, "basic commissioning window opened");
            Ok(())
          }
          Err(err) => {
            // Pre-open close already cleared the deadline on success; keep closed.
            tracing::warn!(error = %err, "open_basic_comm_window failed");
            Err(format!("open pairing window failed: {err}"))
          }
        }
      }
      WindowOp::Close => {
        // Close only windows this bridge opened. If we do not track an open
        // deadline, stay idempotent and do not call close_comm_window — that
        // would revoke a controller-opened ECM window we never tracked.
        if !self.pairing_open() {
          self.mark_window_closed();
          tracing::debug!("close pairing window: no bridge-tracked window; skip stack close");
          Ok(())
        } else {
          match close_comm_window() {
            Ok(was_open) => {
              self.mark_window_closed();
              tracing::info!(was_open, "commissioning window closed");
              Ok(())
            }
            Err(err) => {
              tracing::warn!(error = %err, "close_comm_window failed");
              Err(format!("close pairing window failed: {err}"))
            }
          }
        }
      }
    }
  }

  /// Snapshot fabric count from the live stack; clear our window when a new fabric appears.
  fn sample_fabrics(&self, ctx: &impl HandlerContext) {
    let count = Self::read_fabric_count(ctx);
    self.apply_fabric_sample(count);
  }

  fn read_fabric_count(ctx: &impl HandlerContext) -> u8 {
    let count = ctx.matter().with_state(|state| state.fabrics.iter().count());
    u8::try_from(count).unwrap_or(u8::MAX)
  }
}

/// Bridged Device Basic Information for one slot.
struct BridgedInfoHandler<'a> {
  slot: &'a ExportSlot,
}

impl bridged_info::ClusterHandler for BridgedInfoHandler<'_> {
  const CLUSTER: Cluster<'static> = BRIDGED_INFO_CLUSTER;

  fn dataver(&self) -> u32 {
    self.slot.bridged_info_dataver.load(Ordering::SeqCst)
  }

  fn dataver_changed(&self) {
    self.slot.bridged_info_dataver.fetch_add(1, Ordering::SeqCst);
  }

  fn reachable(&self, _ctx: impl ReadContext) -> Result<bool, Error> {
    Ok(true)
  }

  fn unique_id<P: TLVBuilderParent>(&self, _ctx: impl ReadContext, builder: Utf8StrBuilder<P>) -> Result<P, Error> {
    builder.set(&self.slot.unique_id)
  }

  fn node_label<P: TLVBuilderParent>(&self, _ctx: impl ReadContext, builder: Utf8StrBuilder<P>) -> Result<P, Error> {
    builder.set(self.slot.node_label())
  }

  fn handle_keep_active(
    &self,
    _ctx: impl InvokeContext,
    _request: bridged_info::KeepActiveRequest<'_>,
  ) -> Result<(), Error> {
    // Not advertised: `KeepActive` only applies to ICD-backed bridged devices.
    Err(ErrorCode::CommandNotFound.into())
  }
}

/// OnOff for one slot: reads the slot atomic, writes go to the command queue.
struct SlotOnOffHandler<'a> {
  plane: &'a ExportPlane,
  slot: &'a ExportSlot,
}

impl SlotOnOffHandler<'_> {
  fn set(&self, ctx: impl InvokeContext, on: bool) -> Result<(), Error> {
    if !self.plane.apply_controller_on_off(self.slot.matter_endpoint, on) {
      // The export was withdrawn between dispatch and here.
      return Err(ErrorCode::EndpointNotFound.into());
    }
    // Reports the new value to subscribers and bumps our data version.
    ctx.notify_own_attr_changed(on_off_decl::AttributeId::OnOff as AttrId);
    Ok(())
  }
}

impl on_off_decl::ClusterHandler for SlotOnOffHandler<'_> {
  const CLUSTER: Cluster<'static> = ON_OFF_CLUSTER;

  fn dataver(&self) -> u32 {
    self.slot.functional_dataver.load(Ordering::SeqCst)
  }

  fn dataver_changed(&self) {
    self.slot.functional_dataver.fetch_add(1, Ordering::SeqCst);
  }

  fn on_off(&self, _ctx: impl ReadContext) -> Result<bool, Error> {
    Ok(self.slot.on())
  }

  fn handle_off(&self, ctx: impl InvokeContext) -> Result<(), Error> {
    self.set(ctx, false)
  }

  fn handle_on(&self, ctx: impl InvokeContext) -> Result<(), Error> {
    self.set(ctx, true)
  }

  fn handle_toggle(&self, ctx: impl InvokeContext) -> Result<(), Error> {
    let next = !self.slot.on();
    self.set(ctx, next)
  }

  fn handle_off_with_effect(
    &self,
    _ctx: impl InvokeContext,
    _request: on_off_decl::OffWithEffectRequest<'_>,
  ) -> Result<(), Error> {
    Err(ErrorCode::CommandNotFound.into())
  }

  fn handle_on_with_recall_global_scene(&self, _ctx: impl InvokeContext) -> Result<(), Error> {
    Err(ErrorCode::CommandNotFound.into())
  }

  fn handle_on_with_timed_off(
    &self,
    _ctx: impl InvokeContext,
    _request: on_off_decl::OnWithTimedOffRequest<'_>,
  ) -> Result<(), Error> {
    Err(ErrorCode::CommandNotFound.into())
  }
}

/// Boolean State for a contact-sensor slot.
struct SlotBooleanStateHandler<'a> {
  slot: &'a ExportSlot,
}

impl bool_state_cluster::ClusterHandler for SlotBooleanStateHandler<'_> {
  const CLUSTER: Cluster<'static> = BOOLEAN_STATE_CLUSTER;

  fn dataver(&self) -> u32 {
    self.slot.functional_dataver.load(Ordering::SeqCst)
  }

  fn dataver_changed(&self) {
    self.slot.functional_dataver.fetch_add(1, Ordering::SeqCst);
  }

  fn state_value(&self, _ctx: impl ReadContext) -> Result<bool, Error> {
    Ok(self.slot.contact_closed())
  }
}

/// Occupancy Sensing for a motion-sensor slot.
struct SlotOccupancyHandler<'a> {
  slot: &'a ExportSlot,
}

impl occupancy_cluster::ClusterHandler for SlotOccupancyHandler<'_> {
  const CLUSTER: Cluster<'static> = OCCUPANCY_CLUSTER;

  fn dataver(&self) -> u32 {
    self.slot.functional_dataver.load(Ordering::SeqCst)
  }

  fn dataver_changed(&self) {
    self.slot.functional_dataver.fetch_add(1, Ordering::SeqCst);
  }

  fn occupancy(
    &self,
    _ctx: impl ReadContext,
  ) -> Result<rs_matter::dm::clusters::decl::occupancy_sensing::OccupancyBitmap, Error> {
    Ok(occupancy_cluster::occupancy_bitmap(self.slot.motion_occupied()))
  }

  fn occupancy_sensor_type(
    &self,
    _ctx: impl ReadContext,
  ) -> Result<rs_matter::dm::clusters::decl::occupancy_sensing::OccupancySensorTypeEnum, Error> {
    Ok(occupancy_cluster::sensor_type())
  }

  fn occupancy_sensor_type_bitmap(
    &self,
    _ctx: impl ReadContext,
  ) -> Result<rs_matter::dm::clusters::decl::occupancy_sensing::OccupancySensorTypeBitmap, Error> {
    Ok(occupancy_cluster::sensor_type_bitmap())
  }
}

/// Window Covering for a cover/garage slot.
struct SlotWindowCoveringHandler<'a> {
  plane: &'a ExportPlane,
  slot: &'a ExportSlot,
}

impl SlotWindowCoveringHandler<'_> {
  fn is_garage(&self) -> bool {
    matches!(self.slot.export.type_, DeviceType::Garage)
  }

  fn notify_position_attrs(&self, ctx: impl InvokeContext) -> Result<(), Error> {
    ctx.notify_own_attr_changed(window_covering_cluster::AttributeId::CurrentPositionLiftPercent100ths as AttrId);
    ctx.notify_own_attr_changed(window_covering_cluster::AttributeId::TargetPositionLiftPercent100ths as AttrId);
    ctx.notify_own_attr_changed(window_covering_cluster::AttributeId::OperationalStatus as AttrId);
    Ok(())
  }

  /// Mode write body (no WriteContext). Empty only; non-empty → ConstraintError.
  fn apply_mode_write(&self, value: rs_matter::dm::clusters::decl::window_covering::Mode) -> Result<(), Error> {
    accept_mode_write(value).map_err(Into::into)
  }

  /// Open / close / stop command body (no InvokeContext).
  fn apply_motion_command(&self, kind: CommandKind) -> Result<(), Error> {
    if !self.plane.apply_controller_cover(self.slot.matter_endpoint, kind, None) {
      return Err(ErrorCode::EndpointNotFound.into());
    }
    Ok(())
  }

  /// GoToLiftPercentage body: range check, map to HA scale, enqueue CoverPosition.
  ///
  /// Values outside `0..=10000` return ConstraintError with no slot mutation and
  /// no queued command.
  fn apply_go_to_lift_percentage(&self, percent100ths: u16) -> Result<(), Error> {
    let percent = validate_lift_percent100ths(percent100ths).map_err(Error::from)?;
    let ha_position = percent100ths_to_ha_position(percent);
    if !self
      .plane
      .apply_controller_cover(self.slot.matter_endpoint, CommandKind::CoverPosition, Some(ha_position))
    {
      return Err(ErrorCode::EndpointNotFound.into());
    }
    Ok(())
  }
}

impl window_covering_cluster::ClusterHandler for SlotWindowCoveringHandler<'_> {
  const CLUSTER: Cluster<'static> = WINDOW_COVERING_CLUSTER;

  fn dataver(&self) -> u32 {
    self.slot.functional_dataver.load(Ordering::SeqCst)
  }

  fn dataver_changed(&self) {
    self.slot.functional_dataver.fetch_add(1, Ordering::SeqCst);
  }

  fn r#type(&self, _ctx: impl ReadContext) -> Result<rs_matter::dm::clusters::decl::window_covering::Type, Error> {
    Ok(window_covering_cluster::cover_type(self.is_garage()))
  }

  fn config_status(
    &self,
    _ctx: impl ReadContext,
  ) -> Result<rs_matter::dm::clusters::decl::window_covering::ConfigStatus, Error> {
    Ok(window_covering_cluster::config_status())
  }

  fn operational_status(
    &self,
    _ctx: impl ReadContext,
  ) -> Result<rs_matter::dm::clusters::decl::window_covering::OperationalStatus, Error> {
    Ok(window_covering_cluster::operational_status(
      self.slot.cover_position(),
      self.slot.cover_target(),
      self.slot.cover_moving(),
    ))
  }

  fn end_product_type(
    &self,
    _ctx: impl ReadContext,
  ) -> Result<rs_matter::dm::clusters::decl::window_covering::EndProductType, Error> {
    Ok(window_covering_cluster::end_product_type(self.is_garage()))
  }

  fn current_position_lift_percent_100_ths(&self, _ctx: impl ReadContext) -> Result<Nullable<u16>, Error> {
    Ok(Nullable::some(ha_position_to_percent100ths(self.slot.cover_position())))
  }

  fn target_position_lift_percent_100_ths(&self, _ctx: impl ReadContext) -> Result<Nullable<u16>, Error> {
    Ok(Nullable::some(ha_position_to_percent100ths(self.slot.cover_target())))
  }

  fn mode(&self, _ctx: impl ReadContext) -> Result<rs_matter::dm::clusters::decl::window_covering::Mode, Error> {
    Ok(window_covering_cluster::mode())
  }

  fn set_mode(
    &self,
    _ctx: impl WriteContext,
    value: rs_matter::dm::clusters::decl::window_covering::Mode,
  ) -> Result<(), Error> {
    self.apply_mode_write(value)
  }

  fn handle_up_or_open(&self, ctx: impl InvokeContext) -> Result<(), Error> {
    self.apply_motion_command(CommandKind::CoverOpen)?;
    self.notify_position_attrs(ctx)
  }

  fn handle_down_or_close(&self, ctx: impl InvokeContext) -> Result<(), Error> {
    self.apply_motion_command(CommandKind::CoverClose)?;
    self.notify_position_attrs(ctx)
  }

  fn handle_stop_motion(&self, ctx: impl InvokeContext) -> Result<(), Error> {
    self.apply_motion_command(CommandKind::CoverStop)?;
    self.notify_position_attrs(ctx)
  }

  fn handle_go_to_lift_percentage(
    &self,
    ctx: impl InvokeContext,
    request: rs_matter::dm::clusters::decl::window_covering::GoToLiftPercentageRequest<'_>,
  ) -> Result<(), Error> {
    let percent = request.lift_percent_100_ths_value()?;
    self.apply_go_to_lift_percentage(percent)?;
    self.notify_position_attrs(ctx)
  }

  fn handle_go_to_lift_value(
    &self,
    _ctx: impl InvokeContext,
    _request: rs_matter::dm::clusters::decl::window_covering::GoToLiftValueRequest<'_>,
  ) -> Result<(), Error> {
    Err(ErrorCode::CommandNotFound.into())
  }

  fn handle_go_to_tilt_value(
    &self,
    _ctx: impl InvokeContext,
    _request: rs_matter::dm::clusters::decl::window_covering::GoToTiltValueRequest<'_>,
  ) -> Result<(), Error> {
    Err(ErrorCode::CommandNotFound.into())
  }

  fn handle_go_to_tilt_percentage(
    &self,
    _ctx: impl InvokeContext,
    _request: rs_matter::dm::clusters::decl::window_covering::GoToTiltPercentageRequest<'_>,
  ) -> Result<(), Error> {
    Err(ErrorCode::CommandNotFound.into())
  }
}

impl ExportPlane {
  /// Mark config bumps through `generation` as applied. A concurrent `set_exports`
  /// that advanced past `generation` leaves the newer request pending.
  fn acknowledge_config_generation(&self, generation: u64) {
    let mut applied = self.config_applied.load(Ordering::SeqCst);
    while applied < generation {
      match self
        .config_applied
        .compare_exchange(applied, generation, Ordering::SeqCst, Ordering::SeqCst)
      {
        Ok(_) => break,
        Err(current) => applied = current,
      }
    }
  }
}

#[cfg(test)]
impl ExportPlane {
  /// Whether the stack still owes a `ConfigurationVersion` bump.
  fn config_bump_pending(&self) -> bool {
    self.config_requested.load(Ordering::SeqCst) != self.config_applied.load(Ordering::SeqCst)
  }

  /// Current surface generation requested by `set_exports`.
  fn config_request_generation(&self) -> u64 {
    self.config_requested.load(Ordering::SeqCst)
  }

  /// Stand-in for the successful branch of the run loop's config bump: claim the
  /// latest outstanding generation and acknowledge it.
  fn take_config_bump(&self) -> bool {
    let requested = self.config_requested.load(Ordering::SeqCst);
    let applied = self.config_applied.load(Ordering::SeqCst);
    if requested == applied {
      return false;
    }
    self.acknowledge_config_generation(requested);
    true
  }

  /// Current on/off value of the slot backing `export_id`.
  fn on_for(&self, export_id: Uuid) -> Option<bool> {
    self.snapshot().slot_for(export_id).map(ExportSlot::on)
  }

  fn contact_closed_for(&self, export_id: Uuid) -> Option<bool> {
    self.snapshot().slot_for(export_id).map(ExportSlot::contact_closed)
  }

  fn motion_occupied_for(&self, export_id: Uuid) -> Option<bool> {
    self.snapshot().slot_for(export_id).map(ExportSlot::motion_occupied)
  }

  fn cover_position_for(&self, export_id: Uuid) -> Option<u8> {
    self.snapshot().slot_for(export_id).map(ExportSlot::cover_position)
  }

  fn cover_target_for(&self, export_id: Uuid) -> Option<u8> {
    self.snapshot().slot_for(export_id).map(ExportSlot::cover_target)
  }

  fn cover_percent100ths_for(&self, export_id: Uuid) -> Option<u16> {
    self
      .snapshot()
      .slot_for(export_id)
      .map(|s| ha_position_to_percent100ths(s.cover_position()))
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use std::collections::BTreeMap;

  fn export(id: Uuid, name: &str, entity: &str, type_: DeviceType, enabled: bool, ep: Option<u16>) -> Export {
    Export {
      export_id: id,
      name: name.into(),
      type_,
      primary_entity_id: entity.into(),
      linked: BTreeMap::new(),
      area_id: None,
      enabled,
      endpoint_id: ep,
    }
  }

  fn state(entity: &str, value: &str) -> HaStateValue {
    HaStateValue {
      entity_id: entity.into(),
      state: value.into(),
      attributes: Default::default(),
    }
  }

  #[test]
  fn set_exports_maps_enabled_exports_to_catalog_endpoint_plus_one() {
    let plane = ExportPlane::new();
    let (a, b, c, d) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
    plane.set_exports(&[
      export(a, "Lamp", "light.a", DeviceType::Light, true, Some(4)),
      export(b, "Plug", "switch.b", DeviceType::OnOffPlug, true, Some(1)),
      // Disabled: not bridged.
      export(c, "Off", "light.c", DeviceType::Light, false, Some(7)),
      // Contact is bridged on its own BooleanState endpoint.
      export(d, "Door", "binary_sensor.d", DeviceType::Contact, true, Some(9)),
    ]);

    assert_eq!(
      plane.endpoint_ids(),
      vec![2, 5, 10],
      "catalog endpoint_id + 1, ascending"
    );
  }

  #[test]
  fn set_exports_skips_endpoints_that_collide_with_the_bridge_layout() {
    let plane = ExportPlane::new();
    let (a, b, c) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
    plane.set_exports(&[
      // endpoint_id 0 would land on the aggregator.
      export(a, "Bad", "light.a", DeviceType::Light, true, Some(0)),
      // Never assigned an endpoint.
      export(b, "Unassigned", "light.b", DeviceType::Light, true, None),
      export(c, "Good", "light.c", DeviceType::Light, true, Some(3)),
    ]);
    assert_eq!(plane.endpoint_ids(), vec![4]);
  }

  #[test]
  fn set_exports_preserves_on_state_across_rebuild_for_same_export_id() {
    let plane = ExportPlane::new();
    let (id, other) = (Uuid::new_v4(), Uuid::new_v4());
    plane.set_exports(&[export(id, "Lamp", "light.a", DeviceType::Light, true, Some(1))]);
    assert_eq!(plane.apply_state(id, &[state("light.a", "on")]), 1);
    assert!(plane.on_for(id).unwrap());

    plane.set_exports(&[
      export(id, "Lamp", "light.a", DeviceType::Light, true, Some(1)),
      export(other, "Plug", "switch.b", DeviceType::OnOffPlug, true, Some(2)),
    ]);
    assert!(plane.on_for(id).unwrap(), "on state survives the rebuild");
    assert!(!plane.on_for(other).unwrap(), "new slot starts off");
  }

  #[test]
  fn rebuild_keeps_data_versions_moving_forward() {
    let plane = ExportPlane::new();
    let id = Uuid::new_v4();
    let exports = [export(id, "Lamp", "light.a", DeviceType::Light, true, Some(1))];
    plane.set_exports(&exports);
    let before = plane
      .snapshot()
      .slot_for(id)
      .unwrap()
      .functional_dataver
      .load(Ordering::SeqCst);

    plane.apply_state(id, &[state("light.a", "on")]);
    let bumped = plane
      .snapshot()
      .slot_for(id)
      .unwrap()
      .functional_dataver
      .load(Ordering::SeqCst);
    assert_ne!(before, bumped);

    plane.set_exports(&exports);
    let after = plane
      .snapshot()
      .slot_for(id)
      .unwrap()
      .functional_dataver
      .load(Ordering::SeqCst);
    assert_eq!(bumped, after, "data version must not restart on rebuild");
  }

  #[test]
  fn apply_state_flips_the_slot_and_counts_applied_values() {
    let plane = ExportPlane::new();
    let id = Uuid::new_v4();
    plane.set_exports(&[export(id, "Lamp", "light.a", DeviceType::Light, true, Some(1))]);

    let applied = plane.apply_state(id, &[state("light.a", "on"), state("sensor.battery", "50")]);
    assert_eq!(applied, 2);
    assert!(plane.on_for(id).unwrap());

    assert_eq!(plane.apply_state(id, &[state("light.a", "off")]), 1);
    assert!(!plane.on_for(id).unwrap());

    assert_eq!(plane.apply_state(Uuid::new_v4(), &[state("light.z", "on")]), 0);
  }

  #[test]
  fn apply_state_queues_a_subscription_report_only_when_the_value_moves() {
    let plane = ExportPlane::new();
    let id = Uuid::new_v4();
    plane.set_exports(&[export(id, "Lamp", "light.a", DeviceType::Light, true, Some(1))]);
    plane.drain_reports();

    plane.apply_state(id, &[state("light.a", "on")]);
    assert_eq!(
      plane.drain_reports(),
      vec![(2, ON_OFF_CLUSTER.id, on_off_decl::AttributeId::OnOff as AttrId)]
    );

    plane.apply_state(id, &[state("light.a", "on")]);
    assert!(plane.drain_reports().is_empty(), "unchanged state reports nothing");
  }

  #[test]
  fn renaming_an_export_reports_the_node_label_without_a_config_bump() {
    let plane = ExportPlane::new();
    let id = Uuid::new_v4();
    plane.set_exports(&[export(id, "Lamp", "light.a", DeviceType::Light, true, Some(1))]);
    plane.take_config_bump();
    plane.drain_reports();

    plane.set_exports(&[export(id, "Desk Lamp", "light.a", DeviceType::Light, true, Some(1))]);
    assert!(!plane.config_bump_pending(), "a rename does not change the node shape");
    assert_eq!(
      plane.drain_reports(),
      vec![(
        2,
        BRIDGED_INFO_CLUSTER.id,
        bridged_info::AttributeId::NodeLabel as AttrId
      )]
    );
  }

  #[test]
  fn controller_writes_enqueue_commands_and_ha_writes_do_not() {
    let plane = ExportPlane::new();
    let id = Uuid::new_v4();
    plane.set_exports(&[export(id, "Lamp", "light.a", DeviceType::Light, true, Some(1))]);

    plane.apply_state(id, &[state("light.a", "on")]);
    assert!(
      plane.take_commands().is_empty(),
      "HA state must not echo back as a command"
    );

    assert!(plane.apply_controller_on_off(2, false));
    assert!(!plane.on_for(id).unwrap());
    let cmds = plane.take_commands();
    assert_eq!(cmds.len(), 1);
    assert_eq!(cmds[0].export_id, id);
    assert_eq!(cmds[0].on, Some(false));

    assert!(!plane.apply_controller_on_off(99, true), "unknown endpoint");
    assert!(plane.take_commands().is_empty());
  }

  #[test]
  fn take_commands_drains_the_queue() {
    let plane = ExportPlane::new();
    let id = Uuid::new_v4();
    plane.set_exports(&[export(id, "Lamp", "light.a", DeviceType::Light, true, Some(1))]);
    plane.apply_controller_on_off(2, true);
    plane.apply_controller_on_off(2, false);
    assert_eq!(plane.take_commands().len(), 2);
    assert!(plane.take_commands().is_empty());
  }

  #[test]
  fn config_bump_is_requested_only_when_the_endpoint_set_changes() {
    let plane = ExportPlane::new();
    let id = Uuid::new_v4();
    let exports = [export(id, "Lamp", "light.a", DeviceType::Light, true, Some(1))];

    plane.set_exports(&exports);
    assert!(plane.config_bump_pending(), "first export set changes the surface");
    assert!(plane.take_config_bump());
    assert!(!plane.config_bump_pending());

    plane.set_exports(&exports);
    assert!(!plane.config_bump_pending(), "identical set must not bump");

    plane.set_exports(&[]);
    assert!(
      plane.config_bump_pending(),
      "removing the last endpoint changes the surface"
    );
  }

  #[test]
  fn node_exposes_root_aggregator_and_bridged_endpoints_in_order() {
    let plane = ExportPlane::new();
    plane.set_exports(&[
      export(Uuid::new_v4(), "Lamp", "light.a", DeviceType::Light, true, Some(4)),
      export(Uuid::new_v4(), "Plug", "switch.b", DeviceType::OnOffPlug, true, Some(1)),
    ]);

    plane.access(|node| {
      let ids: Vec<EndptId> = node.endpoints.iter().map(|e| e.id).collect();
      assert_eq!(ids, vec![ROOT_ENDPOINT_ID, AGGREGATOR_ENDPOINT_ID, 2, 5]);

      let dtypes = |ep: EndptId| -> Vec<u16> {
        node
          .endpoint(ep)
          .unwrap()
          .device_types
          .iter()
          .map(|d| d.dtype)
          .collect()
      };

      let aggregator = node.endpoint(AGGREGATOR_ENDPOINT_ID).unwrap();
      assert_eq!(dtypes(AGGREGATOR_ENDPOINT_ID), vec![DEV_TYPE_AGGREGATOR.dtype]);
      assert_eq!(aggregator.clusters.len(), 1, "descriptor only");

      let plug = node.endpoint(2).unwrap();
      assert_eq!(plug.clusters.len(), 3, "descriptor + bridged info + on/off");
      assert_eq!(
        dtypes(2),
        vec![DEV_TYPE_ON_OFF_PLUG_IN_UNIT.dtype, DEV_TYPE_BRIDGED_NODE.dtype]
      );
      assert_eq!(
        dtypes(5),
        vec![DEV_TYPE_ON_OFF_LIGHT.dtype, DEV_TYPE_BRIDGED_NODE.dtype]
      );
    });
  }

  struct Match(Option<EndptId>, Option<ClusterId>);
  impl MatchContext for Match {
    fn endpt(&self) -> Option<EndptId> {
      self.0
    }
    fn cluster(&self) -> Option<ClusterId> {
      self.1
    }
  }

  fn functional_datavers(plane: &ExportPlane) -> Vec<u32> {
    plane
      .snapshot()
      .slots
      .iter()
      .map(|s| s.functional_dataver.load(Ordering::SeqCst))
      .collect()
  }

  #[test]
  fn bump_dataver_visits_every_slot() {
    let plane = ExportPlane::new();
    plane.set_exports(&[
      export(Uuid::new_v4(), "A", "light.a", DeviceType::Light, true, Some(1)),
      export(Uuid::new_v4(), "B", "light.b", DeviceType::Light, true, Some(2)),
    ]);
    let before = functional_datavers(&plane);

    // Wildcard endpoint: every slot must be bumped, not just the first match.
    plane.bump_dataver(Match(None, Some(ON_OFF_CLUSTER.id)));

    assert_eq!(
      functional_datavers(&plane),
      before.iter().map(|v| v.wrapping_add(1)).collect::<Vec<_>>()
    );
  }

  #[test]
  fn bump_dataver_targets_one_endpoint_and_one_cluster() {
    let plane = ExportPlane::new();
    plane.set_exports(&[
      export(Uuid::new_v4(), "A", "light.a", DeviceType::Light, true, Some(1)),
      export(Uuid::new_v4(), "B", "light.b", DeviceType::Light, true, Some(2)),
    ]);
    let before = functional_datavers(&plane);
    let desc_before: Vec<u32> = plane
      .snapshot()
      .slots
      .iter()
      .map(|s| s.desc_dataver.load(Ordering::SeqCst))
      .collect();

    plane.bump_dataver(Match(Some(3), Some(ON_OFF_CLUSTER.id)));

    let after = functional_datavers(&plane);
    assert_eq!(after[0], before[0], "endpoint 2 must not move");
    assert_eq!(after[1], before[1].wrapping_add(1), "endpoint 3 must move");
    assert_eq!(
      plane
        .snapshot()
        .slots
        .iter()
        .map(|s| s.desc_dataver.load(Ordering::SeqCst))
        .collect::<Vec<_>>(),
      desc_before,
      "a different cluster must not move"
    );
  }

  #[test]
  fn matcher_covers_the_aggregator_and_bridged_endpoints_only() {
    assert!(!BridgedEndpointMatcher.matches(Match(Some(ROOT_ENDPOINT_ID), None)));
    assert!(BridgedEndpointMatcher.matches(Match(Some(AGGREGATOR_ENDPOINT_ID), None)));
    assert!(BridgedEndpointMatcher.matches(Match(Some(2), None)));
    assert!(
      BridgedEndpointMatcher.matches(Match(None, None)),
      "wildcard dataver bumps"
    );
  }

  /// The stack thread parks on `changed` inside `futures_lite::block_on`, with
  /// no tokio runtime in scope. This is the one cross-thread assumption the
  /// design rests on, so exercise it exactly the way the stack does.
  #[test]
  fn contact_ha_on_maps_to_state_value_false() {
    let plane = ExportPlane::new();
    let id = Uuid::new_v4();
    plane.set_exports(&[export(
      id,
      "Door",
      "binary_sensor.door",
      DeviceType::Contact,
      true,
      Some(1),
    )]);
    // Default closed = true (Matter closed/normal).
    assert!(plane.contact_closed_for(id).unwrap());

    plane.apply_state(id, &[state("binary_sensor.door", "on")]);
    assert!(
      !plane.contact_closed_for(id).unwrap(),
      "HA on (open) → Matter state_value false"
    );

    plane.apply_state(id, &[state("binary_sensor.door", "off")]);
    assert!(plane.contact_closed_for(id).unwrap());
  }

  #[test]
  fn motion_ha_on_sets_occupied() {
    let plane = ExportPlane::new();
    let id = Uuid::new_v4();
    plane.set_exports(&[export(
      id,
      "PIR",
      "binary_sensor.pir",
      DeviceType::Motion,
      true,
      Some(1),
    )]);
    assert!(!plane.motion_occupied_for(id).unwrap());

    plane.apply_state(id, &[state("binary_sensor.pir", "on")]);
    assert!(plane.motion_occupied_for(id).unwrap());

    plane.drain_reports();
    plane.apply_state(id, &[state("binary_sensor.pir", "on")]);
    assert!(plane.drain_reports().is_empty(), "unchanged motion reports nothing");
  }

  #[test]
  fn cover_apply_state_current_position_37_is_percent100ths_6300() {
    let plane = ExportPlane::new();
    let id = Uuid::new_v4();
    plane.set_exports(&[export(id, "Shade", "cover.shade", DeviceType::Cover, true, Some(1))]);

    let mut attrs = serde_json::Map::new();
    attrs.insert("current_position".into(), serde_json::json!(37));
    let applied = plane.apply_state(
      id,
      &[HaStateValue {
        entity_id: "cover.shade".into(),
        state: "open".into(),
        attributes: attrs,
      }],
    );
    assert_eq!(applied, 1);
    assert_eq!(plane.cover_position_for(id), Some(37));
    assert_eq!(plane.cover_percent100ths_for(id), Some(6300));
    assert_eq!(plane.cover_target_for(id), Some(37), "stopped: current == target");
  }

  /// Handler-level: GoToLiftPercentage 6300 → CoverPosition { position: 37 }.
  #[test]
  fn cover_handler_go_to_lift_6300_enqueues_position_37() {
    let plane = ExportPlane::new();
    let id = Uuid::new_v4();
    plane.set_exports(&[export(id, "Shade", "cover.shade", DeviceType::Cover, true, Some(1))]);
    let state = plane.snapshot();
    let handler = SlotWindowCoveringHandler {
      plane: &plane,
      slot: state.slot_at(2).expect("cover endpoint"),
    };

    handler.apply_go_to_lift_percentage(6300).expect("valid percent100ths");

    let cmds = plane.take_commands();
    assert_eq!(cmds.len(), 1);
    assert_eq!(cmds[0].kind, CommandKind::CoverPosition);
    assert_eq!(cmds[0].position, Some(37));
    assert_eq!(cmds[0].export_id, id);
    assert_eq!(plane.cover_target_for(id), Some(37));
  }

  /// Handler-level: percent100ths 10001 → ConstraintError, no queue, no mutation.
  #[test]
  fn cover_handler_go_to_lift_10001_is_constraint_error_and_queues_nothing() {
    let plane = ExportPlane::new();
    let id = Uuid::new_v4();
    plane.set_exports(&[export(id, "Shade", "cover.shade", DeviceType::Cover, true, Some(1))]);
    let target_before = plane.cover_target_for(id).unwrap();
    let pos_before = plane.cover_position_for(id).unwrap();

    let state = plane.snapshot();
    let handler = SlotWindowCoveringHandler {
      plane: &plane,
      slot: state.slot_at(2).expect("cover endpoint"),
    };

    let err = handler.apply_go_to_lift_percentage(10_001).expect_err("out of range");
    assert_eq!(err.code(), ErrorCode::ConstraintError);
    assert!(
      plane.take_commands().is_empty(),
      "invalid GoToLiftPercentage must not enqueue"
    );
    assert_eq!(plane.cover_target_for(id), Some(target_before));
    assert_eq!(plane.cover_position_for(id), Some(pos_before));
  }

  /// Handler-level: Open / Close / Stop each enqueue exactly one correct command.
  #[test]
  fn cover_handler_open_close_stop_each_enqueue_one_command() {
    let plane = ExportPlane::new();
    let id = Uuid::new_v4();
    plane.set_exports(&[export(id, "Shade", "cover.shade", DeviceType::Cover, true, Some(1))]);
    let state = plane.snapshot();
    let handler = SlotWindowCoveringHandler {
      plane: &plane,
      slot: state.slot_at(2).expect("cover endpoint"),
    };

    handler.apply_motion_command(CommandKind::CoverOpen).expect("open");
    let cmds = plane.take_commands();
    assert_eq!(cmds.len(), 1);
    assert_eq!(cmds[0].kind, CommandKind::CoverOpen);
    assert_eq!(cmds[0].export_id, id);
    assert_eq!(cmds[0].position, None);

    handler.apply_motion_command(CommandKind::CoverClose).expect("close");
    let cmds = plane.take_commands();
    assert_eq!(cmds.len(), 1);
    assert_eq!(cmds[0].kind, CommandKind::CoverClose);

    handler.apply_motion_command(CommandKind::CoverStop).expect("stop");
    let cmds = plane.take_commands();
    assert_eq!(cmds.len(), 1);
    assert_eq!(cmds[0].kind, CommandKind::CoverStop);
  }

  /// Handler-level Mode write: empty accepted; any non-empty bit → ConstraintError.
  #[test]
  fn cover_handler_mode_write_empty_ok_non_empty_rejected() {
    use rs_matter::dm::clusters::decl::window_covering::Mode;

    let plane = ExportPlane::new();
    let id = Uuid::new_v4();
    plane.set_exports(&[export(id, "Shade", "cover.shade", DeviceType::Cover, true, Some(1))]);
    let state = plane.snapshot();
    let handler = SlotWindowCoveringHandler {
      plane: &plane,
      slot: state.slot_at(2).expect("cover endpoint"),
    };

    handler
      .apply_mode_write(Mode::empty())
      .expect("empty Mode is a no-op success");
    assert!(plane.take_commands().is_empty());

    let err = handler
      .apply_mode_write(Mode::MOTOR_DIRECTION_REVERSED)
      .expect_err("unsupported Mode bits");
    assert_eq!(err.code(), ErrorCode::ConstraintError);

    let err = handler
      .apply_mode_write(Mode::CALIBRATION_MODE | Mode::MAINTENANCE_MODE)
      .expect_err("any non-empty Mode");
    assert_eq!(err.code(), ErrorCode::ConstraintError);
    assert!(plane.take_commands().is_empty());
  }

  #[test]
  fn cover_and_contact_state_survive_rebuild() {
    let plane = ExportPlane::new();
    let (cover_id, contact_id) = (Uuid::new_v4(), Uuid::new_v4());
    plane.set_exports(&[
      export(cover_id, "Shade", "cover.shade", DeviceType::Cover, true, Some(1)),
      export(
        contact_id,
        "Door",
        "binary_sensor.door",
        DeviceType::Contact,
        true,
        Some(2),
      ),
    ]);

    let mut attrs = serde_json::Map::new();
    attrs.insert("current_position".into(), serde_json::json!(25));
    plane.apply_state(
      cover_id,
      &[HaStateValue {
        entity_id: "cover.shade".into(),
        state: "open".into(),
        attributes: attrs,
      }],
    );
    plane.apply_state(contact_id, &[state("binary_sensor.door", "on")]);

    plane.set_exports(&[
      export(cover_id, "Shade", "cover.shade", DeviceType::Cover, true, Some(1)),
      export(
        contact_id,
        "Door",
        "binary_sensor.door",
        DeviceType::Contact,
        true,
        Some(2),
      ),
    ]);
    assert_eq!(plane.cover_position_for(cover_id), Some(25));
    assert!(!plane.contact_closed_for(contact_id).unwrap());
  }

  #[test]
  fn node_exposes_contact_motion_and_cover_device_types() {
    let plane = ExportPlane::new();
    plane.set_exports(&[
      export(
        Uuid::new_v4(),
        "Door",
        "binary_sensor.d",
        DeviceType::Contact,
        true,
        Some(1),
      ),
      export(
        Uuid::new_v4(),
        "PIR",
        "binary_sensor.m",
        DeviceType::Motion,
        true,
        Some(2),
      ),
      export(Uuid::new_v4(), "Shade", "cover.s", DeviceType::Cover, true, Some(3)),
    ]);

    plane.access(|node| {
      let dtypes = |ep: EndptId| -> Vec<u16> {
        node
          .endpoint(ep)
          .unwrap()
          .device_types
          .iter()
          .map(|d| d.dtype)
          .collect()
      };
      assert_eq!(
        dtypes(2),
        vec![DEV_TYPE_CONTACT_SENSOR.dtype, DEV_TYPE_BRIDGED_NODE.dtype]
      );
      assert_eq!(
        dtypes(3),
        vec![DEV_TYPE_OCCUPANCY_SENSOR.dtype, DEV_TYPE_BRIDGED_NODE.dtype]
      );
      assert_eq!(
        dtypes(4),
        vec![DEV_TYPE_WINDOW_COVERING.dtype, DEV_TYPE_BRIDGED_NODE.dtype]
      );
      assert_eq!(node.endpoint(2).unwrap().clusters.len(), 3);
      assert_eq!(node.endpoint(4).unwrap().clusters.len(), 3);
    });
  }

  #[test]
  fn a_thread_without_a_tokio_runtime_wakes_on_ha_state() {
    use std::sync::mpsc;
    use std::time::Duration;

    let plane = Arc::new(ExportPlane::new());
    let id = Uuid::new_v4();
    plane.set_exports(&[export(id, "Lamp", "light.a", DeviceType::Light, true, Some(1))]);
    plane.drain_reports();
    plane.take_config_bump();
    // Consume the permit `set_exports` stored for the new endpoint, so what the
    // spawned thread waits on below is only the HA state push.
    futures_lite::future::block_on(plane.changed.notified());

    let (woke_tx, woke_rx) = mpsc::channel();
    let stack_thread = {
      let plane = Arc::clone(&plane);
      std::thread::spawn(move || {
        futures_lite::future::block_on(plane.changed.notified());
        woke_tx.send(()).unwrap();
        plane.drain_reports()
      })
    };

    // Nothing has changed yet, so the waiter must still be parked. Without this
    // the assertions below would also pass on a future that never blocks.
    assert!(
      woke_rx.recv_timeout(Duration::from_millis(150)).is_err(),
      "woke up with no pending change"
    );

    plane.apply_state(id, &[state("light.a", "on")]);

    assert!(
      woke_rx.recv_timeout(Duration::from_secs(5)).is_ok(),
      "HA state did not wake the stack thread"
    );
    assert_eq!(
      stack_thread.join().unwrap(),
      vec![(2, ON_OFF_CLUSTER.id, on_off_decl::AttributeId::OnOff as AttrId)]
    );
  }

  #[test]
  fn node_label_is_clamped_to_the_matter_limit() {
    assert_eq!(clamp_utf8("short", NODE_LABEL_MAX_BYTES), "short");
    let long = "é".repeat(40);
    let clamped = clamp_utf8(&long, NODE_LABEL_MAX_BYTES);
    assert!(clamped.len() <= NODE_LABEL_MAX_BYTES);
    assert_eq!(clamped, "é".repeat(16));
  }

  #[test]
  fn export_slot_exposes_public_name() {
    let plane = ExportPlane::new();
    let id = Uuid::new_v4();
    plane.set_exports(&[export(id, "Kitchen Lamp", "light.a", DeviceType::Light, true, Some(1))]);

    let state = plane.snapshot();
    let slot = state.slot_for(id).unwrap();
    // Tasks 4/5 read the public field; the accessor must stay in lockstep.
    assert_eq!(slot.name, "Kitchen Lamp");
    assert_eq!(slot.name(), "Kitchen Lamp");
  }

  /// Deterministic regression for the AtomicBool load/store race:
  /// run loop loads dirty, concurrent set_exports sets dirty, run loop clears —
  /// the concurrent request must not be lost.
  #[test]
  fn concurrent_set_exports_cannot_drop_a_config_bump() {
    let plane = ExportPlane::new();
    let (a, b) = (Uuid::new_v4(), Uuid::new_v4());

    plane.set_exports(&[export(a, "A", "light.a", DeviceType::Light, true, Some(1))]);
    assert!(plane.config_bump_pending());
    let claimed = plane.config_request_generation();
    assert!(claimed > 0);

    // Surface change while a bump for `claimed` is "in flight".
    plane.set_exports(&[
      export(a, "A", "light.a", DeviceType::Light, true, Some(1)),
      export(b, "B", "light.b", DeviceType::Light, true, Some(2)),
    ]);
    let after = plane.config_request_generation();
    assert!(after > claimed, "later set_exports must advance the generation");
    assert!(plane.config_bump_pending());

    // Acknowledge only the generation the run loop observed before the race.
    // An AtomicBool store(false) would clear both; the generation protocol must not.
    plane.acknowledge_config_generation(claimed);
    assert!(
      plane.config_bump_pending(),
      "a concurrent surface change must still owe a ConfigurationVersion bump"
    );

    // Clearing the latest generation retires the debt.
    assert!(plane.take_config_bump());
    assert!(!plane.config_bump_pending());
  }

  /// Multi-thread stress: writers flip the surface while a stand-in run loop
  /// acknowledges only the generation it observed (never a blind clear).
  #[test]
  fn config_bump_generation_survives_cross_thread_set_exports() {
    use std::sync::Barrier;

    let plane = Arc::new(ExportPlane::new());
    let (a, b) = (Uuid::new_v4(), Uuid::new_v4());
    plane.set_exports(&[export(a, "A", "light.a", DeviceType::Light, true, Some(1))]);
    assert!(plane.take_config_bump());

    let stop = Arc::new(AtomicBool::new(false));
    let barrier = Arc::new(Barrier::new(2));

    let runner = {
      let plane = Arc::clone(&plane);
      let stop = Arc::clone(&stop);
      let barrier = Arc::clone(&barrier);
      std::thread::spawn(move || {
        barrier.wait();
        loop {
          let requested = plane.config_request_generation();
          if plane.config_bump_pending() {
            // In-flight window: yield so writers can interleave a set_exports.
            std::thread::yield_now();
            plane.acknowledge_config_generation(requested);
          } else if stop.load(Ordering::SeqCst) {
            break;
          } else {
            std::thread::yield_now();
          }
        }
        // Final drain after writers stop.
        if plane.config_bump_pending() {
          let requested = plane.config_request_generation();
          plane.acknowledge_config_generation(requested);
        }
      })
    };

    barrier.wait();
    for i in 0..400 {
      if i % 2 == 0 {
        plane.set_exports(&[export(a, "A", "light.a", DeviceType::Light, true, Some(1))]);
      } else {
        plane.set_exports(&[
          export(a, "A", "light.a", DeviceType::Light, true, Some(1)),
          export(b, "B", "switch.b", DeviceType::OnOffPlug, true, Some(2)),
        ]);
      }
    }
    stop.store(true, Ordering::SeqCst);
    runner.join().expect("config bump runner");

    // If any generation were lost, a debt would remain.
    assert!(
      !plane.config_bump_pending(),
      "every surface change must have been acknowledged"
    );
  }

  #[test]
  fn device_type_change_notifies_bridged_endpoint_descriptor() {
    let plane = ExportPlane::new();
    let id = Uuid::new_v4();
    plane.set_exports(&[export(id, "Lamp", "light.a", DeviceType::Light, true, Some(1))]);
    assert!(plane.take_config_bump());
    assert!(plane.drain_reports().is_empty());

    let desc_before = plane
      .snapshot()
      .slot_for(id)
      .unwrap()
      .desc_dataver
      .load(Ordering::SeqCst);

    // Same export_id and endpoint, different concrete device type.
    plane.set_exports(&[export(id, "Lamp", "switch.a", DeviceType::OnOffPlug, true, Some(1))]);

    assert!(plane.config_bump_pending(), "device-type change is a surface change");
    let desc_after = plane
      .snapshot()
      .slot_for(id)
      .unwrap()
      .desc_dataver
      .load(Ordering::SeqCst);
    assert_ne!(desc_before, desc_after, "bridged Descriptor dataver must move");

    assert_eq!(
      plane.drain_reports(),
      vec![(2, DESC_CLUSTER.id, desc::AttributeId::DeviceTypeList as AttrId)],
      "controllers subscribed to the bridged Descriptor must be notified"
    );

    plane.access(|node| {
      let dtypes: Vec<u16> = node.endpoint(2).unwrap().device_types.iter().map(|d| d.dtype).collect();
      assert_eq!(
        dtypes,
        vec![DEV_TYPE_ON_OFF_PLUG_IN_UNIT.dtype, DEV_TYPE_BRIDGED_NODE.dtype]
      );
    });
  }

  #[test]
  fn device_type_stable_across_identical_rebuild_skips_descriptor_report() {
    let plane = ExportPlane::new();
    let id = Uuid::new_v4();
    let exports = [export(id, "Lamp", "light.a", DeviceType::Light, true, Some(1))];
    plane.set_exports(&exports);
    plane.take_config_bump();
    plane.drain_reports();

    let desc_before = plane
      .snapshot()
      .slot_for(id)
      .unwrap()
      .desc_dataver
      .load(Ordering::SeqCst);

    plane.set_exports(&exports);
    assert!(!plane.config_bump_pending());
    assert!(
      plane.drain_reports().is_empty(),
      "stable surface must not re-notify Descriptor"
    );
    assert_eq!(
      plane
        .snapshot()
        .slot_for(id)
        .unwrap()
        .desc_dataver
        .load(Ordering::SeqCst),
      desc_before
    );
  }

  /// Same-iteration fabric-increase + open must not wipe a fresh window: the real
  /// open path baselines fabrics before installing the new deadline.
  #[test]
  fn fabric_increase_then_open_keeps_new_deadline() {
    let plane = ExportPlane::new();
    // Startup: no fabrics, window open.
    plane.note_startup_commissioning_state(0);
    assert!(plane.pairing_open());
    assert_eq!(plane.commissioned_fabrics(), 0);

    // Fabric increase (commissioning completed) clears the old window — mirrors
    // run-loop sample_fabrics before drain_window_request.
    plane.apply_fabric_sample(1);
    assert!(!plane.pairing_open());
    assert_eq!(plane.commissioned_fabrics(), 1);

    // Multi-admin re-open via the real open-path control flow (not mark_window_opened).
    let mut close_calls = 0u32;
    let mut open_calls = 0u32;
    let result = plane.execute_window_op(
      WindowOp::Open(300),
      || 1u8,
      || {
        close_calls += 1;
        Ok(true)
      },
      |_timeout| {
        open_calls += 1;
        Ok(())
      },
    );
    assert!(result.is_ok());
    assert_eq!(close_calls, 1);
    assert_eq!(open_calls, 1);
    assert!(plane.pairing_open());
    // Same fabric count after open must not clear the new deadline.
    plane.apply_fabric_sample(1);
    assert!(
      plane.pairing_open(),
      "baseline before deadline must keep open at the same fabric count"
    );
    assert_eq!(plane.commissioned_fabrics(), 1);
  }

  /// Regression: opening without re-baselining leaves a stale prev; next sample wipes
  /// the fresh open. Documents why the open path stores fabric_count before deadline.
  #[test]
  fn fabric_increase_open_race_without_baseline_would_wipe() {
    let plane = ExportPlane::new();
    plane.note_startup_commissioning_state(0);
    assert_eq!(plane.commissioned_fabrics(), 0);

    // Simulate buggy path: store deadline without updating fabric baseline.
    let deadline = epoch_secs().saturating_add(300);
    plane.window_deadline.store(deadline, Ordering::SeqCst);
    assert!(plane.pairing_open());

    // Fabric count already rose (commissioning done) but prev is still 0.
    plane.apply_fabric_sample(1);
    assert!(
      !plane.pairing_open(),
      "stale prev makes count>prev wipe a deadline set without re-baseline"
    );

    // Correct path: open-path re-baselines before deadline so the same sample is a no-op.
    let result = plane.execute_window_op(WindowOp::Open(300), || 1u8, || Ok(true), |_timeout| Ok(()));
    assert!(result.is_ok());
    assert!(plane.pairing_open());
    plane.apply_fabric_sample(1);
    assert!(plane.pairing_open());
  }

  /// Close-then-open failure when currently closed: close first (clears deadline),
  /// then open fails → final deadline stays zero (no stale pairing_open).
  #[test]
  fn close_success_open_failure_leaves_window_closed() {
    let plane = ExportPlane::new();
    // Fabrics present at startup → no bridge-tracked window (must take close/open path).
    plane.note_startup_commissioning_state(1);
    assert!(!plane.pairing_open());

    let mut close_calls = 0u32;
    let mut open_calls = 0u32;
    let result = plane.execute_window_op(
      WindowOp::Open(300),
      || 1u8,
      || {
        close_calls += 1;
        Ok(true)
      },
      |_timeout| {
        open_calls += 1;
        Err("stack rejected open".into())
      },
    );
    assert!(result.is_err(), "open failure must surface as Err");
    assert_eq!(close_calls, 1, "open path must call stack close first");
    assert_eq!(open_calls, 1, "open path must attempt stack open after close");
    assert!(!plane.pairing_open());
    assert_eq!(
      plane.window_deadline.load(Ordering::SeqCst),
      0,
      "failed open after successful pre-open close must leave deadline zero"
    );
  }

  /// Open while already open: no stack close/reopen thrash; stays open.
  #[test]
  fn open_when_already_open_is_noop() {
    let plane = ExportPlane::new();
    plane.note_startup_commissioning_state(0);
    assert!(plane.pairing_open());
    let deadline_before = plane.window_deadline.load(Ordering::SeqCst);

    let mut close_calls = 0u32;
    let mut open_calls = 0u32;
    let result = plane.execute_window_op(
      WindowOp::Open(300),
      || 0u8,
      || {
        close_calls += 1;
        Ok(true)
      },
      |_timeout| {
        open_calls += 1;
        Ok(())
      },
    );
    assert!(result.is_ok());
    assert_eq!(close_calls, 0, "must not close when window already open");
    assert_eq!(open_calls, 0, "must not reopen when window already open");
    assert!(plane.pairing_open());
    // Deadline is only extended when proposed > current; startup 900s usually wins.
    assert!(
      plane.window_deadline.load(Ordering::SeqCst) >= deadline_before,
      "deadline must not shrink on already-open open"
    );
  }

  /// Close is idempotent when we do not track an open window: stack close is not called.
  #[test]
  fn close_untracked_window_is_idempotent_without_stack() {
    let plane = ExportPlane::new();
    // Fabrics present at startup → no bridge-tracked window.
    plane.note_startup_commissioning_state(1);
    assert!(!plane.pairing_open());

    let mut close_calls = 0u32;
    let mut open_calls = 0u32;
    let result = plane.execute_window_op(
      WindowOp::Close,
      || 1u8,
      || {
        close_calls += 1;
        Ok(true)
      },
      |_timeout| {
        open_calls += 1;
        Ok(())
      },
    );
    assert!(result.is_ok());
    assert_eq!(
      close_calls, 0,
      "close_comm_window must not be called when !pairing_open"
    );
    assert_eq!(open_calls, 0);
    assert!(!plane.pairing_open());
    assert_eq!(plane.commissioned_fabrics(), 1);
  }

  /// Tracked open window: close path does invoke stack close and clears the deadline.
  #[test]
  fn close_tracked_window_calls_stack_close() {
    let plane = ExportPlane::new();
    plane.note_startup_commissioning_state(0);
    assert!(plane.pairing_open());

    let mut close_calls = 0u32;
    let result = plane.execute_window_op(
      WindowOp::Close,
      || 0u8,
      || {
        close_calls += 1;
        Ok(true)
      },
      |_timeout| panic!("open must not be called for Close"),
    );
    assert!(result.is_ok());
    assert_eq!(close_calls, 1);
    assert!(!plane.pairing_open());
    assert_eq!(plane.window_deadline.load(Ordering::SeqCst), 0);
  }

  #[test]
  fn fabric_increase_clears_open_window() {
    let plane = ExportPlane::new();
    plane.note_startup_commissioning_state(0);
    assert!(plane.pairing_open());
    plane.apply_fabric_sample(1);
    assert!(!plane.pairing_open());
    // Further samples at the same count stay closed.
    plane.apply_fabric_sample(1);
    assert!(!plane.pairing_open());
  }
}
