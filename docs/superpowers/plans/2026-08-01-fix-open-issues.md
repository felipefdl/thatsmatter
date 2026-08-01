# Fix Open Issues (#1-#6) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Close GitHub issues #1-#6: live subscription updates, one Matter endpoint per export, per-install pairing material, pairing window control, component lifecycle fixes, and ruff wiring.

**Architecture:** The Rust bridge (rs-matter 0.2 / rs-matter-stack 0.1) grows from one hardcoded OnOff endpoint to a Matter bridge node: aggregator endpoint plus one bridged endpoint per enabled export, composed at runtime by a hand-implemented `Metadata` + `AsyncHandler` pair behind a catch-all matcher. Cross-thread rule: the Matter stack is `Send` but not `Sync`, so every rs-matter call happens on the stack thread; the IPC plane communicates via atomics, `parking_lot::Mutex` tables, and `tokio::sync::Notify` wakes. The HA component gains a pairing button, push-based entities, and service cleanup.

**Tech Stack:** Rust (rs-matter 0.2.0, rs-matter-stack 0.1.x, axum, tokio), Python (Home Assistant custom component, pytest), ruff, just.

## Global Constraints

- REQUIRED READING for every bridge task: the research report at `/private/tmp/claude-501/-Users-felipefdl-Projects-thatsmatter/12042f78-a10b-48de-a1a2-a4fb8f0ca9a7/scratchpad/rs-matter-capabilities.md`. It has exact rs-matter type names, signatures, and `file:line` refs into `~/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/rs-matter-0.2.0` (`$RSM`) and the rs-matter-stack source. Never guess an rs-matter API: use the report, and read the registry source when the report does not cover something.
- rs-matter's `Matter`/`MatterStack` are `Send` but NOT `Sync` (no `sync-mutex` feature). All `matter()`, `with_state`, comm-window, and metadata calls happen on the Matter stack thread only. Cross-thread signaling: atomics + `tokio::sync::Notify` (already a dependency via tokio; its `notified()` future works under `futures_lite::block_on`).
- `mzyy94/daikin-matter` is GPL-3.0. Reading it for the architectural shape is fine; copying code from it is forbidden.
- Rust: edition 2024, rustfmt (`max_width = 120`, 2 spaces), `cargo clippy --all-targets -- -D warnings` must pass, `cargo test` must pass. Run all three before every commit that touches `bridge/`.
- Python: component tests must keep running WITHOUT Home Assistant installed (`custom_components/thatsmatter/tests` imports only pure modules: `helpers`, `models`, `store`). Run `.venv-test/bin/python -m pytest custom_components/thatsmatter/tests -q` and `.venv-test/bin/python -m py_compile custom_components/thatsmatter/*.py` before every commit that touches the component.
- `protocol/schema.json` is the IPC contract. Changes are additive only; update the schema in the same commit as the Rust types it describes.
- Smoke gates: `bash scripts/smoke_ipc.sh` after IPC changes; `bash scripts/smoke_rs_matter.sh` after Matter stack changes. Both must pass before the task's final commit.
- Commits: conventional `type(scope): lowercase subject` under 72 chars, body explains why. The closing commit of each task carries `Fixes #N` on its own line. Never add Co-Authored-By or mention AI tooling.
- Docs and user-facing text: present-tense facts only. No invented product versions ("v1", "in this release"), no roadmap promises. Unsupported means "Not supported".
- Matter endpoint layout (locked): endpoint 0 = root (stack-supplied), endpoint 1 = aggregator, exports at Matter endpoint `catalog endpoint_id + 1`. The +1 offset avoids colliding with the aggregator without migrating persisted catalogs.
- Device type mapping (locked): light -> On/Off Light (0x0100, exists as `DEV_TYPE_ON_OFF_LIGHT`); on_off_switch, on_off_plug, outlet -> On/Off Plug-in Unit (0x010A, declare it); contact -> Contact Sensor (0x0015, declare); motion -> Occupancy Sensor (0x0107, declare); cover, garage -> Window Covering (0x0202, declare). Every bridged endpoint also carries the Bridged Node device type (exists per report §5) and a Bridged Device Basic Information cluster.
- rs-matter sizing (locked): add cargo feature `max-subscriptions-8` on rs-matter (report §10: default is 3, exhausted by HA+Alexa+Google). Events stay disabled: every hand-written cluster uses `.with_events(with!())`.

---

### Task 1: Component lifecycle: unregister services, push entity updates

Closes #4. Component only; no bridge changes.

**Files:**
- Modify: `custom_components/thatsmatter/__init__.py` (unload path)
- Modify: `custom_components/thatsmatter/services.py` (add `async_unregister_services`)
- Modify: `custom_components/thatsmatter/coordinator.py` (notify listeners on state transitions)
- Modify: `custom_components/thatsmatter/sensor.py`, `custom_components/thatsmatter/binary_sensor.py` (listener-driven, no polling)

**Interfaces:**
- Consumes: `ThatsMatterRuntime.add_listener/remove_listener/notify_listeners` (exist today; `image.py` shows the pattern).
- Produces: `async_unregister_services(hass) -> None` in `services.py`; coordinator calls `notify_listeners()` after every connection-state transition and every successful status/pairing refresh.

- [ ] **Step 1: Services unregister.** In `services.py` add:

```python
@callback
def async_unregister_services(hass: HomeAssistant) -> None:
    """Remove domain services (called when the last config entry unloads)."""
    if not hass.data.pop(f"_{DOMAIN}_services", None):
        return
    for service in (
        SERVICE_ADD_EXPORT,
        SERVICE_UPDATE_EXPORT,
        SERVICE_REMOVE_EXPORT,
        SERVICE_SET_ENABLED,
        SERVICE_RESET_NAME_FROM_HA,
    ):
        hass.services.async_remove(DOMAIN, service)
```

In `__init__.py::async_unload_entry`, after popping the runtime: if no other runtime remains in `hass.data[DOMAIN]` (keys not starting with `_`), import and call `async_unregister_services(hass)`.

- [ ] **Step 2: Coordinator notifies on transitions.** In `coordinator.py`: `_command_loop` calls `self.notify_listeners()` whenever `bridge_connected` flips (both directions: after the reconnect branch, and in the `BridgeClientError` handler only when it was previously connected). `_status_loop` already refreshes status+pairing; make `async_refresh_status` call `notify_listeners()` when the status payload or connection state changed since the last call (compare to the previous `bridge_status` dict before overwriting).
- [ ] **Step 3: Entities become push-based.** In `sensor.py` and `binary_sensor.py`: set `_attr_should_poll = False`; add `async_added_to_hass` registering `self._handle_runtime_update` via `self._runtime.add_listener(...)` and `async_will_remove_from_hass` unregistering it, where `_handle_runtime_update` is a `@callback` calling `self.async_write_ha_state()`. Mirror `image.py:60-72`. Put the listener wiring in `ThatsMatterBaseSensor` once; `binary_sensor.py` repeats it locally (no shared base class exists there).
- [ ] **Step 4: Verify.** Run `.venv-test/bin/python -m py_compile custom_components/thatsmatter/*.py` and `.venv-test/bin/python -m pytest custom_components/thatsmatter/tests -q` (38 tests, all pass).
- [ ] **Step 5: Commit** `fix(component): unregister services on unload and push entity updates`, body: why (stale services after removal; 30s entity lag), footer `Fixes #4`.

### Task 2: Per-install pairing material

Closes #3. Bridge + smoke script. Report sections: §7a-§7f.

**Files:**
- Create: `bridge/src/matter/commissioning.rs` (generate/persist/load material)
- Modify: `bridge/src/matter/mod.rs` (add module)
- Modify: `bridge/src/matter/pairing.rs` (derive PairingMaterial from stored material instead of CSA constants)
- Modify: `bridge/src/matter/rs_matter_backend.rs`, `bridge/src/matter/dev.rs` (use stored material)
- Modify: `scripts/smoke_rs_matter.sh` (no hardcoded 20202021/3840)

**Interfaces:**
- Produces: `CommissioningMaterial { pub passcode: u32, pub discriminator: u16 }` with `CommissioningMaterial::load_or_generate(data_dir: &Path) -> anyhow::Result<Self>`; `pairing_material_for(material: &CommissioningMaterial) -> PairingMaterial` in `pairing.rs`. Both backends call `load_or_generate` in `new()` (change `new()` to return `anyhow::Result<Self>`; `main.rs` already propagates anyhow errors).
- Consumes: rs-matter types per report §7: `BasicCommData { password: passcode.to_le_bytes().into(), discriminator }`; verifier is derived by rs-matter at window-open time from the raw passcode (no precompute). Pairing code derivation per §7e; QR payload builder already in `pairing.rs::encode_qr_payload` (generalize it to take the material).

Rules (report §7d: rs-matter does NOT validate passcodes, we must):
- passcode in `1..=99_999_998`, excluding the 12 invalid values: 00000000, 11111111, 22222222, 33333333, 44444444, 55555555, 66666666, 77777777, 88888888, 99999999, 12345678, 87654321.
- discriminator in `0..=4095`.
- Persist to `<data_dir>/commissioning.json` (serde, pretty, atomic write via `.tmp` + rename like `catalog/store.rs::persist`). Loading an existing file with invalid values regenerates (log a warning).
- Keep `TEST_DEV_DET` and `TEST_DEV_ATT` unchanged (report §7f: the CSA example CD binds VID 0xFFF1 / PID 0x8000..=0x8063; custom passcode+discriminator work with them).
- Keep `TEST_PASSCODE`/`TEST_DISCRIMINATOR` consts only if still referenced by tests; update tests to assert against the loaded material instead of the constants.

- [ ] **Step 1: Write failing tests** in `commissioning.rs` `#[cfg(test)]`: generate-then-reload is stable; two different temp dirs produce different passcodes (loop 3 attempts to dodge the astronomically unlikely collision); denylisted/out-of-range persisted values regenerate; discriminator <= 4095.
- [ ] **Step 2: Implement** `commissioning.rs` with `rand::Rng` (already a dependency) and the rules above.
- [ ] **Step 3: Wire both backends + pairing.rs.** `RsMatterBackend::new`/`DevMatterBackend::new` load material and build `PairingMaterial`; `run_matter_stack` receives the material and constructs `BasicCommData` from it (replacing `TEST_DEV_COMM`) per report §7b. Update `pairing.rs` tests.
- [ ] **Step 4: Smoke.** `scripts/smoke_rs_matter.sh`: replace the `passcode == 20202021` / `discriminator == 3840` asserts with: setup_code non-empty, `qr_payload` starts `MT:`, passcode in valid range and not in denylist, and a second bridge start on the SAME data dir returns the identical setup_code. Run `cargo test`, clippy, fmt, `bash scripts/smoke_rs_matter.sh`, `bash scripts/smoke_ipc.sh`.
- [ ] **Step 5: Commit** `feat(bridge): per-install pairing material`, body: shared CSA test passcode is public knowledge; footer `Fixes #3`.

### Task 3: Bridged multi-endpoint node with live subscriptions

Closes #1 (and builds the framework #2 finishes). The single hardcoded OnOff light is replaced by: aggregator endpoint + one bridged endpoint per enabled OnOff-capable export, with subscription notifications when HA pushes state. This is unshipped surface: delete `SharedLight`/`ExportOnOffHooks` outright, no compatibility layer.

Report sections: §3 (notify path, cross-thread hazard), §5 (aggregator, bridged node, `DescHandler::new_aggregator`), §6 (runtime `Node`, hand-implemented `Metadata`+`AsyncHandler`, catch-all matcher, `Box::leak` for the stack), §2b (hand-written handler over generated decl).

**Files:**
- Create: `bridge/src/matter/export_plane.rs` (slot table + `Metadata` + `AsyncHandler` + notify loop)
- Create: `bridge/src/matter/device_types.rs` (missing `DeviceType` consts: 0x010A plug-in unit, 0x0015 contact, 0x0107 occupancy, 0x0202 window covering; public fields per report §4)
- Modify: `bridge/src/matter/rs_matter_backend.rs` (stack composition, set_exports/apply_state/take_commands against the slot table)
- Modify: `bridge/src/matter/mod.rs`
- Modify: `bridge/Cargo.toml` (rs-matter feature `max-subscriptions-8`)

**Interfaces:**
- Produces (consumed by Task 4 and Task 5):

```rust
/// Shared between IPC threads and the Matter stack thread.
pub struct ExportPlane {
  /// Slot table; IPC mutates under lock, stack thread reads in Metadata::access.
  slots: parking_lot::Mutex<Vec<ExportSlot>>,
  /// Wakes the plane's run() loop: emit subscription reports, bump config version.
  changed: tokio::sync::Notify,
  commands: parking_lot::Mutex<VecDeque<CommandRequest>>,
}

pub struct ExportSlot {
  pub export_id: Uuid,
  pub matter_endpoint: u16,          // catalog endpoint_id + 1
  pub name: String,                  // BDBI NodeLabel
  pub kind: SlotKind,                // OnOff { on: AtomicBool } in this task
  // per-slot Datavers for desc/bdbi/functional cluster
}

impl ExportPlane {
  pub fn set_exports(&self, exports: &[Export]);          // rebuild slots, preserve state by export_id
  pub fn apply_state(&self, export_id: Uuid, states: &[HaStateValue]) -> u32;
  pub fn take_commands(&self) -> Vec<CommandRequest>;
}
```

- Consumes: `on_off_map::{on_off_from_states, is_matter_on_off_export}`, `CommandRequest`, catalog `Export`.

Design rules:
- `Metadata::access` builds the `Node` from the current slot table (report §6b): root endpoint (from rs-matter-stack), aggregator endpoint 1 with `DescHandler::new_aggregator` (report §5), then one `Endpoint` per slot with `devices!(bridged-node-type, functional-type)` and `clusters!(desc, BDBI, functional)`. Endpoint array built per-access from the locked table (leaked or arena-backed per report §6; follow the report's recommendation).
- Handler: one concrete struct implementing `AsyncHandler` (`read_awaits/write_awaits/invoke_awaits` return false; dispatch on `ctx.attr().endpoint_id` / `ctx.cmd().endpoint_id`; `bump_dataver` iterates ALL slots without early return, per report §6c). Chain: rs-matter-stack root chain `.chain(catch-all matcher for endpoint >= 1, plane)`. Write the catch-all `Matcher` by hand (report §6c: `EpClMatcher` is a convenience, not a requirement).
- BDBI: hand-written handler over `$GEN/bridged_device_basic_information.rs` decl per report §2b pattern: `reachable() -> true`, `node_label() -> slot.name`, `unique_id() -> export_id` short form if the decl requires it (check the generated trait for the mandatory attribute set; implement exactly those plus node_label).
- OnOff per slot: reuse rs-matter's `OnOffHandler`? No: it binds one endpoint at construction and its hooks are per-instance. Instead implement the OnOff `ClusterHandler` by hand over `$GEN/on_off.rs` decl for the slot (on/off/toggle commands mutate the slot atomic and enqueue `on_off_command(export_id, on)`, exactly like today's `set_from_controller`, including nothing enqueued when the write came from HA).
- Subscriptions (#1): `apply_state` stores new state into the slot atomics, marks the slot's functional Dataver changed, and `self.changed.notify_one()`. The plane's `run(ctx)` loop: `changed.notified().await`, then per report §3b invoke the notify path that schedules subscription reports (exact call per report §3: the `HandlerContext` notification that reports attribute changes; follow §3c's example shape). Controllers subscribed to OnOff then receive reports without polling.
- Config version: every `set_exports` that changes the slot set requests `bump_configuration_version()` via the run loop (report: mandatory on any change to the exposed surface) and bumps the aggregator descriptor Dataver so PartsList updates propagate.
- Stack: replace `StaticCell` with `Box::leak` (report §6d) so the composition is buildable with runtime data; stack thread otherwise unchanged.
- Delete: `SharedLight`, `ExportOnOffHooks`, `LIGHT_ENDPOINT_ID`, `primary_on_off_export` usage in the backend (the function stays in `on_off_map.rs` only if its tests still cover it; otherwise delete it and its tests).

- [ ] **Step 1: Failing unit tests** in `export_plane.rs`: `set_exports` builds one slot per enabled on/off export at `catalog_id + 1` and preserves `on` state across a rebuild for the same `export_id`; `apply_state` flips the atomic and returns applied count; a controller-style invoke enqueues `CommandRequest` while an HA-applied change does not; `take_commands` drains.
- [ ] **Step 2: Implement `export_plane.rs` + `device_types.rs`** per the design rules. Slot rebuild preserves per-export state (`on`) by `export_id`.
- [ ] **Step 3: Recompose the stack** in `rs_matter_backend.rs`: metadata from the plane, handler chain root + plane, run loop wired. `set_exports`/`apply_state`/`take_commands` delegate to the plane.
- [ ] **Step 4: Verify.** `cargo test` (existing dev-backend and IPC tests must stay green), clippy `-D warnings`, fmt, `bash scripts/smoke_rs_matter.sh`, `bash scripts/smoke_ipc.sh`.
- [ ] **Step 5: Commit** `feat(bridge): bridged multi-endpoint node with live subscriptions`, body: one endpoint per export under an aggregator; HA state changes now push subscription reports; footer `Fixes #1`.

### Task 4: Cover, contact, and motion cluster handlers

Closes #2. Extends Task 3's `SlotKind` with the remaining device families. Report sections: §2b (handler template), §2c (per-cluster shape), §1d.

**Files:**
- Create: `bridge/src/matter/clusters/window_covering.rs`, `bridge/src/matter/clusters/boolean_state.rs`, `bridge/src/matter/clusters/occupancy.rs` (+ `clusters/mod.rs`)
- Modify: `bridge/src/matter/export_plane.rs` (new `SlotKind` variants + state mapping)
- Modify: `bridge/src/matter/on_off_map.rs` only if shared state parsing needs a helper (e.g. `ha_cover_position(&HaStateValue) -> Option<u8>` reading `attributes.current_position`)

**Interfaces:**
- Consumes: Task 3's `ExportPlane`, `ExportSlot`, `SlotKind`, command queue; `CommandKind::{CoverPosition, CoverOpen, CoverClose, CoverStop}` (already in the protocol and executed by the component's `_async_execute_command`).
- Produces: `SlotKind::Cover { position: AtomicU8 /* 0-100, 100 = open */, target: AtomicU8, moving: AtomicBool }`, `SlotKind::Contact { closed: AtomicBool }`, `SlotKind::Motion { occupied: AtomicBool }`.

Cluster rules (all three: hand-written `ClusterHandler` over the generated decl, `.with_events(with!())`, sync handlers wrapped `Async(HandlerAdaptor(...))`, per report §2b):
- **BooleanState** (contact): `state_value()` from the slot. Matter semantics: `false` = open/alarm, `true` = closed/normal; map `ha_state_is_on` (HA `on` = open/detected for contact device classes) to `state_value = !on`.
- **OccupancySensing** (motion): `occupancy()` bitmap bit 0 from the slot; `occupancy_sensor_type()` PIR; `occupancy_sensor_type_bitmap()` PIR. Optional attrs stay unimplemented.
- **WindowCovering** (cover, garage): features `LIFT | POSITION_AWARE_LIFT` only. Mandatory reads per report §2c (`type`, `config_status`, `operational_status`, `end_product_type`, current/target lift percent100ths). Commands: `handle_up_or_open` -> enqueue `CommandKind::CoverOpen`, `handle_down_or_close` -> `CoverClose`, `handle_stop_motion` -> `CoverStop`, `handle_go_to_lift_percentage(p)` -> `CoverPosition { position }`. Percent100ths mapping (locked): Matter `0` = fully open, `10000` = fully closed; HA `current_position` `100` = open, `0` = closed; so `percent100ths = (100 - ha_position) * 100`. Tilt commands return unsupported. Position updates from HA set current == target and operational status stopped (report §2c: "instantly at target" unless HA gives intermediate positions; when HA state is `opening`/`closing`, report the matching operational status and keep target at the commanded value).
- State mapping in `apply_state`: cover slots read `attributes.current_position` (fall back: state `open` -> 100, `closed` -> 0); contact/motion via `ha_state_is_on` on the primary entity only.
- Every state change: mark the slot's functional Dataver changed + `changed.notify_one()` so subscriptions fire (same path as Task 3).

- [ ] **Step 1: Failing unit tests**: percent100ths round-trip both directions including endpoints 0/100; `GoToLiftPercentage` enqueues `CoverPosition` with the HA-scale position; `UpOrOpen`/`DownOrClose`/`StopMotion` enqueue the right kinds; contact state maps `on` -> `state_value false`; motion maps `on` -> occupied bit set; `apply_state` on a cover with `current_position: 37` yields percent100ths 6300.
- [ ] **Step 2: Implement the three cluster modules** and the `SlotKind` variants; wire `set_exports` to build slots for `Cover`/`Garage`/`Contact`/`Motion` exports (until now skipped), with device types from `device_types.rs`.
- [ ] **Step 3: Verify.** `cargo test`, clippy, fmt, both smoke scripts.
- [ ] **Step 4: Commit** `feat(bridge): cover, contact, and motion endpoints`, body: every enabled export now has a Matter endpoint; footer `Fixes #2`.

### Task 5: Pairing window control end to end

Closes #6. Bridge (truthful state + open/close), IPC, schema, component button + gating, docs. Report sections: §8 (window control from a chain handler, no auto-reopen, 900s startup window when no fabrics), §9 (fabric introspection via `with_state(|s| s.fabrics.iter())`).

**Files:**
- Modify: `bridge/src/matter/export_plane.rs` or new `bridge/src/matter/control.rs` (window request queue + fabric sampling on the stack thread)
- Modify: `bridge/src/matter/backend.rs` (trait: `open_pairing_window`, `close_pairing_window`), `rs_matter_backend.rs`, `dev.rs`
- Modify: `bridge/src/ipc/handlers.rs`, `bridge/src/ipc/server.rs`, `bridge/src/ipc/types.rs`, `protocol/schema.json`
- Modify: `bridge/tests/export_crud.rs` (window endpoints against the dev backend)
- Create: `custom_components/thatsmatter/button.py`
- Modify: `custom_components/thatsmatter/__init__.py` (`Platform.BUTTON`), `bridge_client.py`, `coordinator.py`, `sensor.py`, `image.py`, `config_flow.py` (pairing step opens the window), `strings.json`, `translations/en.json`
- Modify: `README.md`, `addons/thatsmatter/DOCS.md` (multi-admin pairing: first app uses the printed code; further apps via the button or HA's share flow)
- Modify: `scripts/smoke_ipc.sh` (exercise open/close)

**Interfaces:**
- Backend trait additions:

```rust
/// Open the basic commissioning window for `timeout_secs` (clamped 180..=900).
async fn open_pairing_window(&self, timeout_secs: u16) -> anyhow::Result<()>;
/// Close any window this bridge opened.
async fn close_pairing_window(&self) -> anyhow::Result<()>;
```

- IPC: `POST /pairing/open` body `{"timeout_secs": 300}` (field optional, default 300, clamp 180..=900) -> `200 {"pairing_open": true, "timeout_secs": <clamped>}`; `POST /pairing/close` -> `200 {"pairing_open": false}`. `BridgeStatus` gains `"commissioned_fabrics": <u8>`. Schema updated to match (additive).
- Component: `BridgeClient.open_pairing(timeout_secs: int = 300)`, `close_pairing()`; button entity "Open pairing window" calls `open_pairing` then refreshes status; runtime exposes `pairing_window_open` (from `/status.pairing_open`).

Bridge design (rs_matter backend):
- Window truth (report §8d: rs-matter opens a 900s basic window at startup only when no fabrics exist, never reopens on fabric removal, and window state is not readable): track it ourselves on the stack thread. State: `window_deadline: AtomicU64` (epoch seconds, 0 = closed). Set at startup when fabric count == 0 (deadline = now + 900); set by `open_pairing_window` (now + clamped timeout); cleared by `close_pairing_window`, by deadline expiry, and when the sampled fabric count increases (commissioning completes -> window closed). `pairing_open()` returns `deadline != 0 && now < deadline`. Known limitation, documented in DOCS.md: a window opened by a controller via AdministratorCommissioning (ECM) is not reflected.
- Open/close execution (report §8b): the plane's `run(ctx)` loop already parks on a `Notify`; add a small request mailbox (`Mutex<Option<WindowRequest>>`). `open_pairing_window` posts the request + notifies; the run loop calls `ctx.matter().open_basic_comm_window(secs, ...)` / `close_comm_window` per report §8a signatures, then updates the deadline atomic and samples fabric count into `fabric_count: AtomicU8` via `with_state(|s| s.fabrics.iter().count())` (report §9).
- Dev backend: `open_pairing_window` flips its `pairing_open` bool and spawns a `tokio::time::sleep` task to clear it at the deadline; `close_pairing_window` clears immediately; `commissioned_fabrics` reports 0. Startup keeps `pairing_open = true` for 900s equivalent semantics (set deadline, not a bare bool).

Component behavior:
- Button (`ButtonEntity`, `async_press` -> `runtime.async_open_pairing_window()`, which POSTs, refreshes status, notifies listeners). Device info same as other entities; translation key `open_pairing`.
- Gating: the setup-code sensor's `native_value` and the QR image's `available` return None/False when `pairing_window_open` is false. The pairing notification (`async_show_pairing_notification`) only fires while open. The options-flow pairing step calls `runtime.async_open_pairing_window()` before showing the code, so "Pair with other apps" keeps working with one click and the shown code is actually usable.
- Strings: add button name + pairing step description text mentioning the window duration and that already-paired apps can also share via Home Assistant's Matter "share device" flow.

- [ ] **Step 1: Failing Rust tests** (dev backend + HTTP): `POST /pairing/open` with no body -> 200, status reports `pairing_open true`; with `{"timeout_secs": 60}` -> clamped to 180; `/pairing/close` -> status false; `BridgeStatus` includes `commissioned_fabrics`.
- [ ] **Step 2: Implement bridge side** (trait, dev backend, rs_matter control path, IPC routes, schema).
- [ ] **Step 3: Implement component side** (client methods, runtime helper, button platform, gating, strings, config_flow pairing step).
- [ ] **Step 4: Docs.** README "Use" section and `addons/thatsmatter/DOCS.md`: first app pairs with the printed code while the startup window is open; afterwards press "Open pairing window" (or use HA Matter share) and pair the next app within the window.
- [ ] **Step 5: Verify.** `cargo test`, clippy, fmt, `bash scripts/smoke_ipc.sh` (now exercising open/close), `bash scripts/smoke_rs_matter.sh`, pytest + py_compile for the component.
- [ ] **Step 6: Commit** `feat: pairing window control across bridge and component`, body: window state was hardcoded true and unopenable after first commission; footer `Fixes #6`.

### Task 6: Wire ruff into ha-lint and CI

Closes #5. Tooling only; runs last so it lints the final tree.

**Files:**
- Create: `ruff.toml` (repo root)
- Modify: `justfile` (`ha-lint` recipe, add to `verify`), `.github/workflows/ci.yml` (integration job installs ruff and runs the check)

**Interfaces:** none consumed; produces the lint gate every later change runs under.

- [ ] **Step 1: Config.** `ruff.toml`:

```toml
target-version = "py312"
line-length = 100
include = ["custom_components/thatsmatter/**/*.py", "scripts/*.py"]

[lint]
select = ["E", "F", "W", "I", "B", "UP", "SIM"]
ignore = ["E501"]  # long strings in service/UI text; line-length still guides new code
```

Adjust `ignore` only with a comment saying why each entry exists. Do not add per-file ignores without the same.
- [ ] **Step 2: Install + run.** `.venv-test/bin/python -m pip install ruff`, run `.venv-test/bin/python -m ruff check .`, fix every violation in the component and `scripts/ha_loop_commission.py` (mechanical fixes only; behavior must not change; `git diff` review before commit).
- [ ] **Step 3: Wire.** justfile: `ha-lint: {{python}} -m ruff check .`; `verify: test smoke smoke-matter ha-lint`. CI integration job: `pip install pytest ruff` and a `ruff check .` step before pytest.
- [ ] **Step 4: Verify.** `just ha-lint` clean, pytest green, `git diff` shows no behavior changes.
- [ ] **Step 5: Commit** `chore: wire ruff into ha-lint and ci`, footer `Fixes #5`.

## Verification after all tasks (final gate)

- `cargo test --manifest-path bridge/Cargo.toml`, `cargo clippy --all-targets --manifest-path bridge/Cargo.toml -- -D warnings`, `cargo fmt --check --manifest-path bridge/Cargo.toml`
- `.venv-test/bin/python -m pytest custom_components/thatsmatter/tests -q`, `just ha-lint`
- `bash scripts/smoke_ipc.sh`, `bash scripts/smoke_rs_matter.sh`
- Manual (needs LAN, post-merge): commission with a real controller, verify two on/off exports appear as two devices, toggling in HA updates the controller without refresh, "Open pairing window" makes a second app able to pair.
