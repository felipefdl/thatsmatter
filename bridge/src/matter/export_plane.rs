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
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

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
use rs_matter::tlv::{TLVBuilderParent, Utf8Str, Utf8StrBuilder};
use rs_matter::with;
use rs_matter_stack::eth::EthMatterStack;
use tokio::sync::Notify;
use uuid::Uuid;

use super::device_types::DEV_TYPE_ON_OFF_PLUG_IN_UNIT;
use super::on_off_map::{is_matter_on_off_export, on_off_command, on_off_from_states};
use crate::catalog::{CommandRequest, DeviceType, Export, HaStateValue};

/// Matter endpoint hosting the aggregator (root is 0, bridged devices start at 2).
pub const AGGREGATOR_ENDPOINT_ID: EndptId = 1;

/// Matter caps `NodeLabel` at 32 octets.
const NODE_LABEL_MAX_BYTES: usize = 32;

/// Pending-report bits drained by the stack thread on every wake.
const REPORT_ON_OFF: u32 = 1 << 0;
const REPORT_NODE_LABEL: u32 = 1 << 1;

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

static AGGREGATOR_DEVICE_TYPES: [MatterDeviceType; 1] = [DEV_TYPE_AGGREGATOR];
static AGGREGATOR_CLUSTERS: [Cluster<'static>; 1] = [DESC_CLUSTER];
static BRIDGED_ON_OFF_CLUSTERS: [Cluster<'static>; 3] = [DESC_CLUSTER, BRIDGED_INFO_CLUSTER, ON_OFF_CLUSTER];
// Concrete device type first, `Bridged Node` second, matching upstream's bridge example.
static ON_OFF_LIGHT_DEVICE_TYPES: [MatterDeviceType; 2] = [DEV_TYPE_ON_OFF_LIGHT, DEV_TYPE_BRIDGED_NODE];
static ON_OFF_PLUG_DEVICE_TYPES: [MatterDeviceType; 2] = [DEV_TYPE_ON_OFF_PLUG_IN_UNIT, DEV_TYPE_BRIDGED_NODE];

/// Functional surface a slot exposes to controllers.
#[derive(Debug)]
pub enum SlotKind {
  /// On/off-capable export (light, switch, plug, outlet).
  OnOff { on: AtomicBool },
}

/// One bridged endpoint: a catalog export plus its Matter-side state.
#[derive(Debug)]
pub struct ExportSlot {
  pub export_id: Uuid,
  /// Catalog `endpoint_id` + 1, so the aggregator keeps endpoint 1.
  pub matter_endpoint: EndptId,
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
    &self.export.name
  }

  /// Current on/off value as controllers should see it.
  pub fn on(&self) -> bool {
    match &self.kind {
      SlotKind::OnOff { on } => on.load(Ordering::SeqCst),
    }
  }

  /// Store a new on/off value; returns `true` when it actually changed.
  fn store_on(&self, value: bool) -> bool {
    match &self.kind {
      SlotKind::OnOff { on } => on.swap(value, Ordering::SeqCst) != value,
    }
  }

  fn node_label(&self) -> Utf8Str<'_> {
    clamp_utf8(self.name(), NODE_LABEL_MAX_BYTES)
  }

  fn device_types(&self) -> &'static [MatterDeviceType] {
    match self.export.type_ {
      DeviceType::Light => &ON_OFF_LIGHT_DEVICE_TYPES,
      _ => &ON_OFF_PLUG_DEVICE_TYPES,
    }
  }

  fn endpoint(&self) -> Endpoint<'static> {
    Endpoint::new(self.matter_endpoint, self.device_types(), &BRIDGED_ON_OFF_CLUSTERS)
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
  /// Wakes `run()`: emit subscription reports, bump the configuration version.
  changed: Notify,
  commands: Mutex<VecDeque<CommandRequest>>,
  /// Set when the exposed endpoint set changed and the stack owes a config bump.
  config_dirty: AtomicBool,
  aggregator_dataver: AtomicU32,
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
      config_dirty: AtomicBool::new(false),
      aggregator_dataver: AtomicU32::new(rand::random()),
    }
  }

  /// Rebuild the slot table from the catalog, preserving per-export state.
  ///
  /// Slot identity is the `export_id`: on/off value and data versions carry
  /// over so controllers never see an endpoint go backwards.
  pub fn set_exports(&self, exports: &[Export]) {
    let mut bridged: Vec<&Export> = exports.iter().filter(|e| is_matter_on_off_export(e)).collect();
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
      self.config_dirty.store(true, Ordering::SeqCst);
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
        // Known but not bridged yet (sensor / cover types).
        return (states.len() as u32, false);
      };

      if let Some(on) = on_off_from_states(&slot.export, states)
        && slot.store_on(on)
      {
        // Value first, data version second: a read that races us returns the new
        // value with the old version, never the reverse.
        slot.functional_dataver.fetch_add(1, Ordering::SeqCst);
        slot.request_report(REPORT_ON_OFF);
        tracing::debug!(%export_id, on, endpoint = slot.matter_endpoint, "HA state applied to Matter OnOff");
        return (states.len() as u32, true);
      }
      (states.len() as u32, false)
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
        if bits & REPORT_ON_OFF != 0 {
          out.push((
            slot.matter_endpoint,
            ON_OFF_CLUSTER.id,
            on_off_decl::AttributeId::OnOff as AttrId,
          ));
        }
        if bits & REPORT_NODE_LABEL != 0 {
          out.push((
            slot.matter_endpoint,
            BRIDGED_INFO_CLUSTER.id,
            bridged_info::AttributeId::NodeLabel as AttrId,
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
    let slot = Self {
      export_id: export.export_id,
      matter_endpoint,
      kind: SlotKind::OnOff {
        on: AtomicBool::new(previous.is_some_and(ExportSlot::on)),
      },
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
      // The endpoint's DeviceTypeList moved, so its Descriptor did too.
      slot.desc_dataver.fetch_add(1, Ordering::SeqCst);
    }
    slot
  }
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

  // No `write`: the only writable attribute we advertise is `NodeLabel`, and the
  // catalog owns names, so the default `AttributeNotFound` is the right answer.

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
      id if id == ON_OFF_CLUSTER.id => Handler::read(
        &on_off_decl::HandlerAdaptor(SlotOnOffHandler { plane: self, slot }),
        ctx,
        reply,
      ),
      _ => Err(ErrorCode::ClusterNotFound.into()),
    }
  }

  async fn invoke(&self, ctx: impl InvokeContext, reply: impl InvokeReply) -> Result<(), Error> {
    let (endpoint, cluster) = (ctx.cmd().endpoint_id, ctx.cmd().cluster_id);
    if cluster != ON_OFF_CLUSTER.id {
      return Err(ErrorCode::CommandNotFound.into());
    }

    let state = self.snapshot();
    let Some(slot) = state.slot_at(endpoint) else {
      return Err(ErrorCode::EndpointNotFound.into());
    };
    Handler::invoke(
      &on_off_decl::HandlerAdaptor(SlotOnOffHandler { plane: self, slot }),
      ctx,
      reply,
    )
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
        if hits_cluster(ON_OFF_CLUSTER.id) {
          slot.functional_dataver.fetch_add(1, Ordering::SeqCst);
        }
      }
    });
  }

  /// Turns off-thread changes into data-model notifications. Runs forever on the
  /// stack thread, which is the only place `Matter` may be touched.
  async fn run(&self, ctx: impl HandlerContext) -> Result<(), Error> {
    loop {
      self.changed.notified().await;

      if self.config_dirty.load(Ordering::SeqCst) {
        match ctx.matter().bump_configuration_version(ctx.kv(), &ctx) {
          Ok(version) => {
            // Cleared only on success, so a failed persist retries on the next change.
            self.config_dirty.store(false, Ordering::SeqCst);
            tracing::info!(
              configuration_version = version,
              endpoints = ?self.endpoint_ids(),
              "bridged endpoint set changed"
            );
          }
          Err(err) => tracing::warn!(error = ?err, "configuration version bump failed; retrying on the next change"),
        }
        // PartsList moved on the root endpoint and on the aggregator.
        ctx.notify_cluster_changed(ROOT_ENDPOINT_ID, DESC_CLUSTER.id);
        ctx.notify_cluster_changed(AGGREGATOR_ENDPOINT_ID, DESC_CLUSTER.id);
      }

      for (endpoint, cluster, attr) in self.drain_reports() {
        ctx.notify_attr_changed(endpoint, cluster, attr);
      }
    }
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

#[cfg(test)]
impl ExportPlane {
  /// Whether the stack still owes a `ConfigurationVersion` bump.
  fn config_bump_pending(&self) -> bool {
    self.config_dirty.load(Ordering::SeqCst)
  }

  /// Stand-in for the successful branch of the run loop's config bump.
  fn take_config_bump(&self) -> bool {
    self.config_dirty.swap(false, Ordering::SeqCst)
  }

  /// Current on/off value of the slot backing `export_id`.
  fn on_for(&self, export_id: Uuid) -> Option<bool> {
    self.snapshot().slot_for(export_id).map(ExportSlot::on)
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
  fn set_exports_maps_enabled_on_off_exports_to_catalog_endpoint_plus_one() {
    let plane = ExportPlane::new();
    let (a, b, c, d) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
    plane.set_exports(&[
      export(a, "Lamp", "light.a", DeviceType::Light, true, Some(4)),
      export(b, "Plug", "switch.b", DeviceType::OnOffPlug, true, Some(1)),
      // Disabled: not bridged.
      export(c, "Off", "light.c", DeviceType::Light, false, Some(7)),
      // Not on/off capable: not bridged yet.
      export(d, "Door", "binary_sensor.d", DeviceType::Contact, true, Some(9)),
    ]);

    assert_eq!(plane.endpoint_ids(), vec![2, 5], "catalog endpoint_id + 1, ascending");
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
}
