# ThatsMatter product spec

## What it is

ThatsMatter is a **Matter bridge for Home Assistant**. It turns selected Home Assistant entities into Matter devices so external controllers can control them over the local network.

Target controllers:

- Amazon Alexa
- Google Home
- Samsung SmartThings
- Apple Home
- Any other Matter controller on the LAN

It is **not** a Matter controller. Official Home Assistant Matter already covers devices into HA. ThatsMatter is the other direction: HA out to Matter, same job HomeKit Bridge does for Apple, but multi-ecosystem via Matter.

## Goals

1. Local only. No cloud account for the bridge itself.
2. Opt-in only. Empty catalog by default. Nothing is exposed until the user adds it.
3. Best-in-class export UX: correct **name**, **type**, and **entity set** per device controllers will see.
4. Setup and day-to-day config fully inside Home Assistant. No separate admin website.
5. Minimal surface. Prefer a small, correct catalog over flooding controllers.

## Non-goals

- Replacing the official Matter integration (controller path)
- CSA certification as a product requirement for personal / open use
- Cameras, complex lock credential management, or full cluster coverage on day one
- A standalone web app for configuration
- Automatic “expose everything” modes as the primary UX

## User problem

People want Zigbee, Z-Wave, ESPHome, and other HA devices available in Alexa / Google / SmartThings without cloud skills or per-vendor bridges.

Existing Matter hub / bridge tools for HA mostly use **filters** (domain, label, pattern). That is easy to ship and poor for real homes:

- Wrong name on the controller
- Wrong device type (switch vs light vs outlet vs fan)
- Too many entities (diagnostics, batteries, power sensors nobody asked for)
- Hard to see what is actually exposed

ThatsMatter treats exposure as a **curated catalog**, not a filter dump.

## Mental model

| Concept | Meaning |
|---|---|
| **Bridge** | One Matter bridge node on the LAN (pairing code / QR, fabric state) |
| **Export** | One device as controllers see it (stable identity, name, type) |
| **Binding** | HA entity (or entities) that back that export |
| **Presentation** | Name and Matter type advertised to controllers |

Rules:

- Export identity is **not** the HA `entity_id`. Each export has a stable `export_id`.
- Remapping the HA entity or renaming the Matter name does not create a new export unless the user deletes and recreates it.
- Domain / label / area include is **bootstrap import only**, never the daily management model.

## Setup (fully in HA)

1. Install ThatsMatter (custom integration + bridge process packaging as decided later).
2. Add the integration via Settings → Devices & services (config flow).
3. Bridge is created. HA shows **Matter setup code** and **QR** on the integration page.
4. In Alexa / Google Home / SmartThings / Apple Home, add a Matter device and scan the code.
5. Catalog starts empty. User adds exports.
6. Controllers discover bridged devices as endpoints under the bridge (per Matter bridge behavior).

No separate setup website. Controllers still use their own apps only for the commission step (protocol requirement).

Network expectations (same class as HomeKit / Matter):

- IPv6 and mDNS must work between HA host and controllers
- Host networking or equivalent multicast path
- Document firewall / Docker caveats

## UX

### Bridge page

- Bridge name (LAN / Matter node name)
- Pairing material: manual code + QR
- Status: running, pairing open, error
- Actions: show code, reset pairings / fabric (destructive, confirmed), reload

Copy that should stay visible:

- Local only. No cloud account for ThatsMatter.
- Nothing is exposed until you add it to the catalog.
- You set the name and type each controller sees.
- Controllers may keep their own name after first pairing.

### Catalog (Configure UI)

Primary path is the integration **Configure** options flow (no YAML, no Developer Tools services):

1. **Add devices to export** — HA entity multi-select + optional type override  
2. **Manage exported devices** — rename, type, enable/disable, remove  
3. **Pair with other apps** — setup code + link to QR image entity  
4. Connection settings (host/port) for advanced installs  

Services remain available for automations; they are not required for normal use.

This list (and Manage UI) is the answer to “what is exposed?”

### Add flow

1. **Configure → Add devices to export**  
2. Pick one or more entities (lights, switches, covers, contact/motion sensors)  
3. Optional Matter type (default Automatic)  
4. Submit — each entity becomes an export with friendly-name defaults  

### Export editor (core product)

**Identity**

- Name in Matter (required). Independent of HA friendly name.
- Area (optional override; default from HA).
- `export_id` read-only.

**Type**

- Dropdown of implemented Matter device types only.
- Domain / device_class only sets the default. Always editable.

**Bindings**

- Primary entity (required): drives main capability (on/off, position, …).
- Optional linked entities when supported (e.g. battery), **off by default** for diagnostics and power.
- Prefer device-centric picker when the entity belongs to an HA device: show sibling entities as optional checkboxes, not auto-export all.

**Preview**

One line, e.g. `Alexa / Google will see: Kitchen Lamp · Light · on/off + brightness`

Validation flags:

- Missing primary entity
- Type incompatible with entity domain
- Empty name

**Name controls**

- Default name: HA friendly name (or device name when that is clearer).
- “Reset name from Home Assistant” copies current HA name again.
- Changing HA friendly name later does **not** silently overwrite Matter name (avoids surprise renames on controllers).

### Defaults on create

| HA source | Default Matter name | Default type |
|---|---|---|
| `light.*` | Friendly name | Light |
| `switch.*` with outlet device class | Friendly name | Outlet |
| `switch.*` / `input_boolean.*` | Friendly name | On/Off switch or plug (pick one default; document it) |
| `cover.*` garage/gate | Friendly name | Garage door |
| `cover.*` otherwise | Friendly name | Window covering |
| `binary_sensor` contact/door/window | Friendly name | Contact |
| `binary_sensor` motion/occupancy | Friendly name | Motion |
| `sensor` temperature | Friendly name | Temperature |
| `sensor` humidity | Friendly name | Humidity |
| `climate.*` | Friendly name | Thermostat (when implemented) |

Unsupported domains: reject at add time with a clear reason.

## Architecture (target)

```
HA entities ── state / services ──► Python custom_component (ThatsMatter)
                                         │  config flow, catalog, QR UI
                                         │  export store under .storage
                                         ▼
                                    IPC / local API
                                         │
                              Bridge process (prefer Rust / rs-matter)
                              Matter Bridge node + dynamic endpoints
                                         │
                                    mDNS / IPv6 LAN
                                         ▼
                         Alexa / Google / SmartThings / Apple / …
```

Rationale:

- HA integrations are Python. Matter stack is heavy. Keep protocol work in a dedicated process.
- Rust (`rs-matter`) is the preferred protocol implementation when viable.
- Python owns UX, entity mapping, and HA lifecycle.

Packaging:

- Custom integration for UI and config (`custom_components/thatsmatter`)
- Bridge as HAOS App under `homeassistant/thatsmatter` (host network); package with `scripts/package-haos.sh`

### Export storage (logical)

Per export, at minimum:

```text
export_id          stable id
name               Matter advertised name
type               Matter device type key
primary_entity_id  HA entity
linked             optional map of role → entity_id
area_id            optional override
enabled            bool
endpoint_id        Matter endpoint id (assigned by bridge; stable once set)
```

Bridge-level:

```text
bridge_name
pairing / fabric material (server-side, not in git)
port / network options as needed
```

## Device type scope

### First ship

- Light (on/off, brightness; color when entity supports it)
- On/Off switch / plug / outlet
- Contact binary sensor
- Motion / occupancy binary sensor
- Cover (blind/shade position and/or open-close)
- Garage cover when device class fits

### Next

- Temperature / humidity sensors
- Thermostat (`climate`)
- Fan
- Simple lock (lock/unlock only)

### Not supported (until explicitly added)

- Cameras
- Media players / TVs
- Full lock credential / user management
- Vacuum, siren, and other sparse mappings

## Controller behavior (document for users)

- First commission freezes a lot of name/room behavior on the controller.
- Renaming in ThatsMatter may not rename the device inside Alexa/Google until the user renames there or removes and re-adds.
- Soft disable should stop advertising or mark unavailable cleanly.
- Hard delete of an export may leave a ghost on the controller until removed there.

This is controller behavior. ThatsMatter should explain it, not pretend it owns the controller’s UI.

## Safety and trust (product copy)

ThatsMatter is the project name. Keep tone plain and controlled:

- Opt-in catalog
- Local only
- Explicit name and type
- Destructive actions confirmed

Do not imply official Home Assistant core, CSA certification, or cloud security guarantees the product does not provide.

Avoid naming collisions with official **Matter** (controller) and community **HA Matter Hub** projects. ThatsMatter is a distinct product name; one-liners should say “Matter bridge” as the role, not “HA Matter.”

## Success criteria

A user can:

1. Install and pair the bridge into Alexa or Google Home from material shown in HA.
2. Add a light and a switch as two exports with custom names and types.
3. See only those two devices on the controller, not the rest of the house.
4. Disable one export without deleting its config and have it stop being useful on the controller.
5. Answer “what is exposed?” from the catalog alone.

## Implementation order

1. Spike: bridge process, one fixed On/Off endpoint, commission into a real controller.
2. Dynamic endpoints + HA WebSocket state/command loop for a few types.
3. Custom component: config flow, pairing UI, export store, catalog + editor.
4. Packaging for HAOS.
5. Widen device type map.

## Open decisions

- Exact Matter device type enum and cluster map per HA domain
- Single bridge only vs multiple bridges (controller accessory limits)
- Whether the bridge process is add-on-first or binary-next-to-integration-first
- How aggressively to support color / level / position edge cases in the first ship set

These do not block writing the UX or the spike.
