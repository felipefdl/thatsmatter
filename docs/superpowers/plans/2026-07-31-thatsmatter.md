# ThatsMatter Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Ship a local Matter bridge for Home Assistant that exposes an opt-in export catalog (stable `export_id`) to LAN controllers via a Rust bridge process and a Python `custom_components/thatsmatter` integration for all setup and day-to-day UX.

**Architecture:** Home Assistant owns UX, config flow, and the export store under `.storage`. A dedicated Rust process under `bridge/` speaks Matter on the LAN and serves a loopback HTTP JSON IPC API. The custom component is the only writer of catalog state; the bridge applies catalog snapshots, reports pairing material and runtime status, and maps HA state/commands through IPC. Prefer `rs-matter` for a real OnOff endpoint; if that is not viable after a real compile-and-run attempt, keep a `MatterBackend` trait plus `DevMatterBackend` so catalog, pairing placeholders, and HA UX still work end to end.

**Tech Stack:** Python 3.12+ (Home Assistant custom integration), Rust 2021 edition (`tokio`, `axum` or `hyper` for IPC, `serde`/`serde_json`, `uuid`, optional `rs-matter`), JSON Schema + OpenAPI for the IPC contract, `pytest` for Python, `cargo test` for Rust.

## Global Constraints

- Product name: **ThatsMatter**. Integration domain and package: `thatsmatter`. Repo paths use that name only.
- Export identity is stable `export_id` (UUID string). Never use HA `entity_id` as Matter identity.
- Opt-in catalog only. Empty catalog by default. No "expose everything" primary path.
- First ship device types only: `light`, `on_off_switch`, `on_off_plug`, `outlet`, `contact`, `motion`, `cover`, `garage`.
- IPC is local HTTP JSON on `127.0.0.1` only. Document Unix socket as a future option; do not implement it in this plan.
- Prefer `rs-matter`. After a real attempt, if it does not compile or cannot host a basic OnOff device, implement `MatterBackend` + `DevMatterBackend` and document the gap in `bridge/README.md`. Do not invent dual product versions.
- American English for all surface copy, commits, docs, comments. No em dash characters (`—`). No invented product `v1` / `v2` language.
- All setup and catalog management stay inside Home Assistant. No separate admin website.
- Destructive actions (fabric reset, export delete) require confirmation in HA UX.
- Spec source of truth: `docs/product-spec.md`. This plan does not change product goals.

---

## File map (monorepo)

```text
thatsmatter/
├── README.md
├── docs/
│   ├── product-spec.md
│   └── superpowers/plans/2026-07-31-thatsmatter.md   # this plan
├── protocol/
│   ├── schema.json          # JSON Schema for IPC request/response bodies
│   ├── openapi.yaml         # OpenAPI 3 description of loopback HTTP API
│   └── README.md            # human summary of endpoints and auth model
├── bridge/                  # Rust Matter bridge process
│   ├── Cargo.toml
│   ├── README.md
│   ├── src/
│   │   ├── main.rs          # process entry, CLI flags, logging
│   │   ├── config.rs        # bind address, data dir, bridge name
│   │   ├── ipc/
│   │   │   ├── mod.rs
│   │   │   ├── server.rs    # axum router on 127.0.0.1
│   │   │   ├── handlers.rs  # HTTP handlers
│   │   │   └── types.rs     # serde types matching protocol/
│   │   ├── catalog/
│   │   │   ├── mod.rs
│   │   │   ├── model.rs     # Export, DeviceType, Binding
│   │   │   └── store.rs     # in-memory catalog + endpoint_id assignment
│   │   ├── matter/
│   │   │   ├── mod.rs
│   │   │   ├── backend.rs   # MatterBackend trait
│   │   │   ├── dev.rs       # DevMatterBackend (placeholders)
│   │   │   └── rs_matter_backend.rs  # real backend when viable
│   │   └── state.rs         # shared AppState (catalog, backend, pairing)
│   └── tests/
│       ├── ipc_api.rs
│       └── catalog.rs
├── custom_components/
│   └── thatsmatter/
│       ├── __init__.py
│       ├── manifest.json
│       ├── const.py
│       ├── config_flow.py
│       ├── strings.json
│       ├── services.yaml
│       ├── bridge_client.py  # HTTP client to bridge IPC
│       ├── store.py          # HA .storage export store
│       ├── models.py         # Export dataclass / TypedDict
│       ├── device_types.py   # allowed types + HA defaults
│       ├── coordinator.py    # state sync HA <-> bridge
│       ├── entity.py         # diagnostic / status entities if needed
│       └── quality_scale.yaml (optional later)
├── tests/
│   ├── protocol/
│   │   └── test_schema_examples.py
│   ├── bridge/              # optional thin wrappers; prefer cargo tests
│   └── component/
│       ├── conftest.py
│       ├── test_store.py
│       ├── test_device_types.py
│       ├── test_bridge_client.py
│       └── test_config_flow.py
└── scripts/
    ├── run_bridge.sh
    └── check_protocol.sh    # validate schema + openapi examples
```

Responsibility boundaries:

| Path | Owns | Does not own |
|---|---|---|
| `protocol/` | Wire contract only | Runtime behavior, HA UX |
| `bridge/` | Matter node, endpoint lifecycle, pairing material, IPC server | HA entity registry, UI |
| `custom_components/thatsmatter/` | Config flow, catalog CRUD, export store, bridge lifecycle, HA state/services | Matter cluster encoding |
| `tests/` | Cross-cutting and Python tests | Replacing `cargo test` for bridge unit logic |

---

### Task 1: Protocol package skeleton and device type enum

**Files:**
- Create: `protocol/README.md`
- Create: `protocol/schema.json`
- Create: `protocol/openapi.yaml`

**Interfaces:**
- Consumes: device type list and export fields from `docs/product-spec.md`.
- Produces: JSON Schema definitions `DeviceType`, `Export`, `CatalogSnapshot`, `BridgeStatus`, `PairingMaterial`, `HaStateUpdate`, `CommandRequest`, `CommandResult` used by Tasks 2–8.

- [ ] **Step 1: Write `protocol/README.md`**

Content requirements (exact topics, American English, no em dash):

1. Title: "ThatsMatter IPC"
2. Transport: HTTP/1.1 JSON on `127.0.0.1` only. Default port `18465` (document as configurable). No TLS on loopback.
3. Auth model: shared secret token in header `X-ThatsMatter-Token` required on every request after bootstrap; token is random, written to a file only the HA process and bridge process can read.
4. Who is client: HA custom component is HTTP client; bridge is server.
5. Catalog ownership: HA is source of truth; bridge accepts full catalog replace via `PUT /v1/catalog`.
6. Future: optional Unix socket is out of scope; do not implement.
7. Link to `schema.json` and `openapi.yaml`.

- [ ] **Step 2: Write `protocol/schema.json`**

Root object with `$schema`, `$id` `https://thatsmatter.local/protocol/schema.json`, and `definitions` for:

```json
{
  "$schema": "https://json-schema.org/draft/2020-12/schema",
  "$id": "https://thatsmatter.local/protocol/schema.json",
  "title": "ThatsMatter IPC",
  "definitions": {
    "DeviceType": {
      "type": "string",
      "enum": [
        "light",
        "on_off_switch",
        "on_off_plug",
        "outlet",
        "contact",
        "motion",
        "cover",
        "garage"
      ]
    },
    "LinkedRole": {
      "type": "string",
      "enum": ["battery", "brightness", "position"]
    },
    "Export": {
      "type": "object",
      "required": [
        "export_id",
        "name",
        "type",
        "primary_entity_id",
        "enabled"
      ],
      "additionalProperties": false,
      "properties": {
        "export_id": {
          "type": "string",
          "format": "uuid",
          "description": "Stable export identity. Never the HA entity_id."
        },
        "name": { "type": "string", "minLength": 1, "maxLength": 64 },
        "type": { "$ref": "#/definitions/DeviceType" },
        "primary_entity_id": { "type": "string", "minLength": 1 },
        "linked": {
          "type": "object",
          "additionalProperties": { "type": "string" },
          "description": "Map of LinkedRole to entity_id. Empty object default."
        },
        "area_id": { "type": ["string", "null"] },
        "enabled": { "type": "boolean" },
        "endpoint_id": {
          "type": ["integer", "null"],
          "minimum": 1,
          "description": "Matter endpoint id assigned by bridge; null until assigned."
        }
      }
    },
    "CatalogSnapshot": {
      "type": "object",
      "required": ["exports", "bridge_name"],
      "additionalProperties": false,
      "properties": {
        "bridge_name": { "type": "string", "minLength": 1, "maxLength": 32 },
        "exports": {
          "type": "array",
          "items": { "$ref": "#/definitions/Export" }
        }
      }
    },
    "BridgeStatus": {
      "type": "object",
      "required": [
        "running",
        "matter_backend",
        "pairing_open",
        "export_count",
        "enabled_export_count",
        "error"
      ],
      "additionalProperties": false,
      "properties": {
        "running": { "type": "boolean" },
        "matter_backend": {
          "type": "string",
          "enum": ["rs_matter", "dev"]
        },
        "pairing_open": { "type": "boolean" },
        "export_count": { "type": "integer", "minimum": 0 },
        "enabled_export_count": { "type": "integer", "minimum": 0 },
        "error": { "type": ["string", "null"] }
      }
    },
    "PairingMaterial": {
      "type": "object",
      "required": ["manual_code", "qr_payload", "discriminator", "passcode"],
      "additionalProperties": false,
      "properties": {
        "manual_code": { "type": "string" },
        "qr_payload": {
          "type": "string",
          "description": "Matter QR payload string for rendering in HA"
        },
        "discriminator": { "type": "integer" },
        "passcode": { "type": "integer" }
      }
    },
    "HaStateValue": {
      "type": "object",
      "required": ["entity_id", "state"],
      "additionalProperties": false,
      "properties": {
        "entity_id": { "type": "string" },
        "state": { "type": "string" },
        "attributes": {
          "type": "object",
          "additionalProperties": true
        }
      }
    },
    "HaStateUpdate": {
      "type": "object",
      "required": ["states"],
      "additionalProperties": false,
      "properties": {
        "states": {
          "type": "array",
          "items": { "$ref": "#/definitions/HaStateValue" }
        }
      }
    },
    "CommandKind": {
      "type": "string",
      "enum": [
        "on_off",
        "level",
        "cover_position",
        "cover_open",
        "cover_close",
        "cover_stop"
      ]
    },
    "CommandRequest": {
      "type": "object",
      "required": ["export_id", "kind"],
      "additionalProperties": false,
      "properties": {
        "export_id": { "type": "string", "format": "uuid" },
        "kind": { "$ref": "#/definitions/CommandKind" },
        "on": { "type": "boolean" },
        "level": { "type": "integer", "minimum": 0, "maximum": 254 },
        "position": { "type": "integer", "minimum": 0, "maximum": 100 }
      }
    },
    "CommandResult": {
      "type": "object",
      "required": ["accepted", "export_id", "kind"],
      "additionalProperties": false,
      "properties": {
        "accepted": { "type": "boolean" },
        "export_id": { "type": "string", "format": "uuid" },
        "kind": { "$ref": "#/definitions/CommandKind" },
        "error": { "type": ["string", "null"] }
      }
    },
    "ErrorBody": {
      "type": "object",
      "required": ["error", "message"],
      "additionalProperties": false,
      "properties": {
        "error": { "type": "string" },
        "message": { "type": "string" }
      }
    }
  }
}
```

- [ ] **Step 3: Write `protocol/openapi.yaml`**

OpenAPI 3.0.3 document with:

| Method | Path | Request | Response 200 |
|---|---|---|---|
| GET | `/v1/health` | none | `{ "ok": true, "version": string }` |
| GET | `/v1/status` | none | `BridgeStatus` |
| GET | `/v1/pairing` | none | `PairingMaterial` |
| POST | `/v1/pairing/open` | empty object | `PairingMaterial` |
| POST | `/v1/pairing/close` | empty object | `BridgeStatus` |
| POST | `/v1/fabric/reset` | `{ "confirm": true }` | `BridgeStatus` |
| GET | `/v1/catalog` | none | `CatalogSnapshot` |
| PUT | `/v1/catalog` | `CatalogSnapshot` | `CatalogSnapshot` (with `endpoint_id` filled) |
| POST | `/v1/ha/state` | `HaStateUpdate` | `{ "applied": integer }` |
| GET | `/v1/commands/pending` | none | `{ "commands": CommandRequest[] }` |
| POST | `/v1/commands/{id}/ack` | `CommandResult` | `{ "ok": true }` |

Security scheme: API key header `X-ThatsMatter-Token`.

`$ref` bodies must align with `schema.json` definitions (inline or file ref). Include one example for each of: empty catalog, one light export, pairing material with placeholder values for `DevMatterBackend`.

- [ ] **Step 4: Verify protocol files exist and parse**

Run:

```bash
python3 -c "import json; json.load(open('protocol/schema.json'))"
python3 -c "import pathlib; p=pathlib.Path('protocol/openapi.yaml'); assert p.stat().st_size>200; print('ok', p)"
test -f protocol/README.md && echo README_ok
```

Expected: JSON loads; openapi size check prints `ok`; `README_ok`.

- [ ] **Step 5: Commit**

```bash
git add protocol/
git commit -m "feat(protocol): IPC schema and OpenAPI for ThatsMatter"
```

---

### Task 2: Protocol example fixtures and Python schema smoke tests

**Files:**
- Create: `protocol/examples/catalog_empty.json`
- Create: `protocol/examples/catalog_light.json`
- Create: `protocol/examples/pairing_dev.json`
- Create: `protocol/examples/ha_state_light_on.json`
- Create: `tests/protocol/test_schema_examples.py`
- Create: `scripts/check_protocol.sh`
- Create: `requirements-dev.txt` (jsonschema, pytest, pyyaml)

**Interfaces:**
- Consumes: `protocol/schema.json` definitions from Task 1.
- Produces: validated example payloads re-used by Rust integration tests and HA client tests.

- [ ] **Step 1: Write example JSON files**

`protocol/examples/catalog_empty.json`:

```json
{
  "bridge_name": "ThatsMatter",
  "exports": []
}
```

`protocol/examples/catalog_light.json`:

```json
{
  "bridge_name": "ThatsMatter",
  "exports": [
    {
      "export_id": "11111111-1111-4111-8111-111111111111",
      "name": "Kitchen Lamp",
      "type": "light",
      "primary_entity_id": "light.kitchen_lamp",
      "linked": {},
      "area_id": "kitchen",
      "enabled": true,
      "endpoint_id": null
    }
  ]
}
```

`protocol/examples/pairing_dev.json`:

```json
{
  "manual_code": "0000-000-0000",
  "qr_payload": "MT:Y.K9042C00KA0648G00",
  "discriminator": 3840,
  "passcode": 20202021
}
```

`protocol/examples/ha_state_light_on.json`:

```json
{
  "states": [
    {
      "entity_id": "light.kitchen_lamp",
      "state": "on",
      "attributes": {
        "brightness": 200
      }
    }
  ]
}
```

- [ ] **Step 2: Write `tests/protocol/test_schema_examples.py`**

```python
from __future__ import annotations

import json
from pathlib import Path

import jsonschema
import pytest

ROOT = Path(__file__).resolve().parents[2]
SCHEMA = json.loads((ROOT / "protocol" / "schema.json").read_text())
EXAMPLES = ROOT / "protocol" / "examples"


def _validator(def_name: str) -> jsonschema.Draft202012Validator:
    resolver = jsonschema.RefResolver.from_schema(SCHEMA)
    return jsonschema.Draft202012Validator(
        SCHEMA["definitions"][def_name], resolver=resolver
    )


@pytest.mark.parametrize(
    "filename,def_name",
    [
        ("catalog_empty.json", "CatalogSnapshot"),
        ("catalog_light.json", "CatalogSnapshot"),
        ("pairing_dev.json", "PairingMaterial"),
        ("ha_state_light_on.json", "HaStateUpdate"),
    ],
)
def test_example_matches_schema(filename: str, def_name: str) -> None:
    data = json.loads((EXAMPLES / filename).read_text())
    _validator(def_name).validate(data)


def test_export_requires_export_id_not_entity_id() -> None:
    bad = {
        "bridge_name": "ThatsMatter",
        "exports": [
            {
                "name": "X",
                "type": "light",
                "primary_entity_id": "light.x",
                "enabled": True,
            }
        ],
    }
    with pytest.raises(jsonschema.ValidationError):
        _validator("CatalogSnapshot").validate(bad)


def test_device_type_rejects_unknown() -> None:
    bad = json.loads((EXAMPLES / "catalog_light.json").read_text())
    bad["exports"][0]["type"] = "camera"
    with pytest.raises(jsonschema.ValidationError):
        _validator("CatalogSnapshot").validate(bad)
```

- [ ] **Step 3: Write `requirements-dev.txt` and install**

```text
pytest>=8.0
jsonschema>=4.22
PyYAML>=6.0
aiohttp>=3.9
```

Run:

```bash
python3 -m pip install -r requirements-dev.txt
```

- [ ] **Step 4: Write `scripts/check_protocol.sh`**

```bash
#!/usr/bin/env bash
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"
python3 -c "import json; json.load(open('protocol/schema.json'))"
python3 -c "import yaml; yaml.safe_load(open('protocol/openapi.yaml'))"
python3 -m pytest tests/protocol -q
echo "protocol ok"
```

```bash
chmod +x scripts/check_protocol.sh
./scripts/check_protocol.sh
```

Expected: `protocol ok` and pytest green.

- [ ] **Step 5: Commit**

```bash
git add protocol/examples tests/protocol scripts/check_protocol.sh requirements-dev.txt
git commit -m "test(protocol): schema examples and smoke tests"
```

---

### Task 3: Rust bridge crate skeleton and config

**Files:**
- Create: `bridge/Cargo.toml`
- Create: `bridge/README.md`
- Create: `bridge/src/main.rs`
- Create: `bridge/src/config.rs`
- Create: `bridge/src/lib.rs`

**Interfaces:**
- Consumes: default port `18465` and token path concept from Task 1.
- Produces: binary `thatsmatter-bridge`; `Config { bind: SocketAddr, data_dir: PathBuf, token: String, bridge_name: String, matter_backend: BackendKind }`.

- [ ] **Step 1: Create `bridge/Cargo.toml`**

```toml
[package]
name = "thatsmatter-bridge"
version = "0.1.0"
edition = "2021"
description = "ThatsMatter Matter bridge process"
license = "MIT"
publish = false

[[bin]]
name = "thatsmatter-bridge"
path = "src/main.rs"

[lib]
name = "thatsmatter_bridge"
path = "src/lib.rs"

[dependencies]
anyhow = "1"
axum = "0.7"
clap = { version = "4", features = ["derive", "env"] }
serde = { version = "1", features = ["derive"] }
serde_json = "1"
tokio = { version = "1", features = ["full"] }
tower-http = { version = "0.5", features = ["trace"] }
tracing = "0.1"
tracing-subscriber = { version = "0.3", features = ["env-filter"] }
uuid = { version = "1", features = ["serde", "v4"] }
thiserror = "1"
parking_lot = "0.12"
hex = "0.4"

[dev-dependencies]
http-body-util = "0.1"
tower = { version = "0.5", features = ["util"] }
reqwest = { version = "0.12", default-features = false, features = ["json", "rustls-tls"] }
tokio = { version = "1", features = ["full", "test-util"] }
```

Do not add `rs-matter` until Task 6 proves it builds.

- [ ] **Step 2: Write `bridge/src/config.rs`**

```rust
use std::net::SocketAddr;
use std::path::PathBuf;

use clap::{Parser, ValueEnum};

#[derive(Debug, Clone, Copy, ValueEnum, PartialEq, Eq)]
pub enum BackendKind {
    /// Placeholder backend: full IPC and catalog, no real Matter radio stack.
    Dev,
    /// Real Matter stack when enabled at compile/runtime after Task 6.
    RsMatter,
}

#[derive(Debug, Clone, Parser)]
#[command(name = "thatsmatter-bridge", about = "ThatsMatter Matter bridge")]
pub struct Config {
    /// Loopback HTTP bind address (must be 127.0.0.1 or ::1).
    #[arg(long, env = "THATSMATTER_BIND", default_value = "127.0.0.1:18465")]
    pub bind: SocketAddr,

    /// Directory for fabric material and runtime state.
    #[arg(long, env = "THATSMATTER_DATA_DIR", default_value = "./data")]
    pub data_dir: PathBuf,

    /// Shared secret token. Prefer file via --token-file in production.
    #[arg(long, env = "THATSMATTER_TOKEN")]
    pub token: Option<String>,

    /// Path to token file (single line). Overrides empty --token when set.
    #[arg(long, env = "THATSMATTER_TOKEN_FILE")]
    pub token_file: Option<PathBuf>,

    /// Default bridge name advertised in catalog until HA sets one.
    #[arg(long, env = "THATSMATTER_BRIDGE_NAME", default_value = "ThatsMatter")]
    pub bridge_name: String,

    #[arg(long, env = "THATSMATTER_MATTER_BACKEND", value_enum, default_value_t = BackendKind::Dev)]
    pub matter_backend: BackendKind,
}

impl Config {
    pub fn resolve_token(&self) -> anyhow::Result<String> {
        if let Some(t) = &self.token {
            if !t.is_empty() {
                return Ok(t.clone());
            }
        }
        if let Some(path) = &self.token_file {
            let raw = std::fs::read_to_string(path)?;
            let t = raw.trim().to_string();
            if t.is_empty() {
                anyhow::bail!("token file is empty: {}", path.display());
            }
            return Ok(t);
        }
        anyhow::bail!("token required via --token or --token-file");
    }

    pub fn ensure_loopback(&self) -> anyhow::Result<()> {
        if !self.bind.ip().is_loopback() {
            anyhow::bail!("bind address must be loopback, got {}", self.bind);
        }
        Ok(())
    }
}
```

- [ ] **Step 3: Write `bridge/src/lib.rs` and minimal `main.rs`**

`lib.rs`:

```rust
pub mod config;

pub use config::{BackendKind, Config};
```

`main.rs`:

```rust
use clap::Parser;
use thatsmatter_bridge::Config;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env().add_directive("info".parse()?))
        .init();

    let cfg = Config::parse();
    cfg.ensure_loopback()?;
    let token = cfg.resolve_token()?;
    tracing::info!(
        bind = %cfg.bind,
        backend = ?cfg.matter_backend,
        token_len = token.len(),
        "thatsmatter-bridge starting (IPC not yet bound; next tasks)"
    );
    // Task 5 binds the IPC server.
    Ok(())
}
```

- [ ] **Step 4: Write `bridge/README.md`**

Sections:

1. What the bridge process does
2. Build: `cargo build --manifest-path bridge/Cargo.toml`
3. Run with `--token` on loopback
4. Matter backend: default `dev`; `rs-matter` path described as preferred when Task 6 succeeds
5. Gap section header reserved: "Matter stack status" (fill after Task 6)

- [ ] **Step 5: Verify**

```bash
cd bridge && cargo test && cargo build
./target/debug/thatsmatter-bridge --token testtoken --bind 127.0.0.1:18465
```

Expected: builds; process starts and exits cleanly (until Task 5 keeps it running). Reject non-loopback:

```bash
./target/debug/thatsmatter-bridge --token t --bind 0.0.0.0:18465; echo exit:$?
```

Expected: non-zero exit.

- [ ] **Step 6: Commit**

```bash
git add bridge/
git commit -m "feat(bridge): crate skeleton and loopback config"
```

---

### Task 4: Catalog model and endpoint assignment in Rust

**Files:**
- Create: `bridge/src/catalog/mod.rs`
- Create: `bridge/src/catalog/model.rs`
- Create: `bridge/src/catalog/store.rs`
- Create: `bridge/tests/catalog.rs`
- Modify: `bridge/src/lib.rs` (add `pub mod catalog;`)

**Interfaces:**
- Consumes: `DeviceType` and `Export` fields from protocol.
- Produces:
  - `pub enum DeviceType { Light, OnOffSwitch, OnOffPlug, Outlet, Contact, Motion, Cover, Garage }` with serde rename to snake_case protocol values.
  - `pub struct Export { export_id: Uuid, name: String, type_: DeviceType, primary_entity_id: String, linked: BTreeMap<String, String>, area_id: Option<String>, enabled: bool, endpoint_id: Option<u16> }`
  - `pub struct CatalogSnapshot { bridge_name: String, exports: Vec<Export> }`
  - `CatalogStore::replace(snapshot) -> CatalogSnapshot` that assigns stable `endpoint_id` starting at `1`, reuses previous mapping by `export_id`, frees ids on delete.

- [ ] **Step 1: Write failing integration test `bridge/tests/catalog.rs`**

```rust
use thatsmatter_bridge::catalog::{CatalogSnapshot, CatalogStore, DeviceType, Export};
use uuid::Uuid;

#[test]
fn replace_assigns_endpoint_ids_and_preserves_by_export_id() {
    let mut store = CatalogStore::new("ThatsMatter");
    let id_a = Uuid::parse_str("11111111-1111-4111-8111-111111111111").unwrap();
    let id_b = Uuid::parse_str("22222222-2222-4222-8222-222222222222").unwrap();

    let snap = CatalogSnapshot {
        bridge_name: "Home".into(),
        exports: vec![
            Export {
                export_id: id_a,
                name: "A".into(),
                type_: DeviceType::Light,
                primary_entity_id: "light.a".into(),
                linked: Default::default(),
                area_id: None,
                enabled: true,
                endpoint_id: None,
            },
            Export {
                export_id: id_b,
                name: "B".into(),
                type_: DeviceType::Outlet,
                primary_entity_id: "switch.b".into(),
                linked: Default::default(),
                area_id: None,
                enabled: true,
                endpoint_id: None,
            },
        ],
    };

    let out = store.replace(snap).unwrap();
    assert_eq!(out.bridge_name, "Home");
    let ep_a = out.exports.iter().find(|e| e.export_id == id_a).unwrap().endpoint_id;
    let ep_b = out.exports.iter().find(|e| e.export_id == id_b).unwrap().endpoint_id;
    assert!(ep_a.is_some() && ep_b.is_some());
    assert_ne!(ep_a, ep_b);

    // Drop B, keep A: A's endpoint_id must stay the same.
    let snap2 = CatalogSnapshot {
        bridge_name: "Home".into(),
        exports: vec![Export {
            export_id: id_a,
            name: "A renamed".into(),
            type_: DeviceType::Light,
            primary_entity_id: "light.a2".into(),
            linked: Default::default(),
            area_id: Some("kitchen".into()),
            enabled: false,
            endpoint_id: None, // client may send null; store keeps prior
        }],
    };
    let out2 = store.replace(snap2).unwrap();
    assert_eq!(out2.exports.len(), 1);
    assert_eq!(out2.exports[0].endpoint_id, ep_a);
    assert_eq!(out2.exports[0].name, "A renamed");
    assert!(!out2.exports[0].enabled);
}
```

- [ ] **Step 2: Run test (expect fail)**

```bash
cd bridge && cargo test --test catalog
```

Expected: FAIL (module missing).

- [ ] **Step 3: Implement catalog modules**

Implement `model.rs` with serde derives matching protocol enums exactly (`light`, `on_off_switch`, ...).

Implement `store.rs`:

```rust
// Core logic sketch (complete the file fully when implementing):
pub struct CatalogStore {
    bridge_name: String,
    by_id: BTreeMap<Uuid, Export>,
    next_endpoint: u16, // start at 1
}

impl CatalogStore {
    pub fn new(bridge_name: impl Into<String>) -> Self { /* ... */ }

    pub fn snapshot(&self) -> CatalogSnapshot { /* ... */ }

    pub fn replace(&mut self, incoming: CatalogSnapshot) -> anyhow::Result<CatalogSnapshot> {
        // 1. set bridge_name
        // 2. for each export: if known export_id, reuse endpoint_id; else alloc next_endpoint
        // 3. drop exports not present
        // 4. reject empty name, unknown types already handled by serde
        // 5. return snapshot with endpoint_id Some
    }
}
```

Endpoint id rules:

- Range: `1..=u16::MAX` (document Matter dynamic endpoint practical limits in comments).
- Never reuse an id while any export still holds it; after free, may reuse lowest free id or continue monotonic. Prefer **monotonic** `next_endpoint` for simplicity in this plan.
- Identity key is only `export_id`.

- [ ] **Step 4: Run tests**

```bash
cd bridge && cargo test --test catalog
```

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add bridge/
git commit -m "feat(bridge): catalog store with stable export_id endpoint map"
```

---

### Task 5: MatterBackend trait and DevMatterBackend

**Files:**
- Create: `bridge/src/matter/mod.rs`
- Create: `bridge/src/matter/backend.rs`
- Create: `bridge/src/matter/dev.rs`
- Create: `bridge/src/state.rs`
- Create: `bridge/tests/dev_backend.rs`
- Modify: `bridge/src/lib.rs`

**Interfaces:**
- Consumes: `CatalogSnapshot`, `Export`, `DeviceType`.
- Produces:

```rust
#[async_trait::async_trait]
pub trait MatterBackend: Send + Sync {
    fn kind(&self) -> BackendKind;
    async fn start(&self) -> anyhow::Result<()>;
    async fn apply_catalog(&self, snapshot: &CatalogSnapshot) -> anyhow::Result<()>;
    async fn apply_ha_state(&self, states: &[HaStateValue]) -> anyhow::Result<u32>;
    async fn pairing_material(&self) -> anyhow::Result<PairingMaterial>;
    async fn open_pairing(&self) -> anyhow::Result<PairingMaterial>;
    async fn close_pairing(&self) -> anyhow::Result<()>;
    async fn reset_fabric(&self) -> anyhow::Result<()>;
    async fn take_pending_commands(&self) -> Vec<(String, CommandRequest)>; // id, cmd
    async fn status_error(&self) -> Option<String>;
}
```

`DevMatterBackend`:

- Generates deterministic-looking placeholder `PairingMaterial` (document as not commissionable).
- Tracks catalog and HA state in memory.
- On `open_pairing` / `close_pairing` toggles a bool.
- `reset_fabric` clears pairing-open and any in-memory fabric placeholder file under data_dir.
- Never binds Matter ports.

Also define shared IPC serde types in `bridge/src/ipc/types.rs` matching protocol (can create types.rs here even if server comes next).

Add dependency if needed:

```toml
async-trait = "0.1"
```

- [ ] **Step 1: Write failing test for pairing placeholders**

```rust
use thatsmatter_bridge::matter::dev::DevMatterBackend;
use thatsmatter_bridge::matter::MatterBackend;

#[tokio::test]
async fn dev_backend_returns_pairing_placeholders() {
    let backend = DevMatterBackend::new("./target/tmp-dev-data");
    backend.start().await.unwrap();
    let p = backend.pairing_material().await.unwrap();
    assert!(!p.manual_code.is_empty());
    assert!(!p.qr_payload.is_empty());
    let p2 = backend.open_pairing().await.unwrap();
    assert_eq!(p.manual_code, p2.manual_code);
}
```

- [ ] **Step 2: Implement trait + DevMatterBackend + state shell**

`AppState`:

```rust
pub struct AppState {
    pub token: String,
    pub catalog: parking_lot::Mutex<CatalogStore>,
    pub backend: Arc<dyn MatterBackend>,
}
```

- [ ] **Step 3: Run tests**

```bash
cd bridge && cargo test
```

Expected: PASS.

- [ ] **Step 4: Commit**

```bash
git add bridge/
git commit -m "feat(bridge): MatterBackend trait and DevMatterBackend"
```

---

### Task 6: rs-matter spike (real attempt) or documented fallback

**Files:**
- Modify: `bridge/Cargo.toml` (optional feature `rs-matter`)
- Create: `bridge/src/matter/rs_matter_backend.rs` (or stub that fails compile only under feature)
- Modify: `bridge/README.md` section "Matter stack status"
- Modify: `bridge/src/matter/mod.rs`

**Interfaces:**
- Produces either:
  - **Path A (preferred):** `RsMatterBackend` implementing `MatterBackend` that commissions as a Matter bridge with at least one OnOff endpoint when catalog has an enabled on/off export; or
  - **Path B:** Feature remains off; `BackendKind::RsMatter` returns a clear runtime error; README documents compile/run failure notes and that HA UX uses `dev` backend fully.

- [ ] **Step 1: Research and attempt dependency**

```bash
# From bridge/, try adding rs-matter (crates.io or git). Record exact source.
cargo search rs-matter
```

Attempt minimal compile of an OnOff device example from upstream docs or examples. Capture:

- crate source URL and revision
- compile result (success/fail + first error)
- whether dynamic endpoints / bridged devices are supported enough for first ship

Timebox: one focused spike session. Do not multi-day thrash inside this task.

- [ ] **Step 2A (if viable): Implement `RsMatterBackend`**

Minimum behavior:

- Start Matter stack on host network interfaces as required by `rs-matter`
- Advertise bridge node
- Expose pairing material from real stack
- Map enabled exports of types `on_off_switch`, `on_off_plug`, `outlet`, and on/off portion of `light` to OnOff cluster
- Other first-ship types may still be catalog-only until later tasks extend clusters

Wire `BackendKind::RsMatter` in `main` to construct this backend.

- [ ] **Step 2B (if not viable): Document and keep Dev path complete**

Update `bridge/README.md`:

```markdown
## Matter stack status

Attempted `rs-matter` on <date>: <source> @ <rev>.

Result: <did not compile | compiled but no OnOff path | ...>.

Primary runtime backend for development and HA UX integration is `DevMatterBackend`
(`--matter-backend dev`). It implements full catalog IPC and returns non-commissionable
pairing placeholders so the Home Assistant UI can be built and tested.

Revisit `rs-matter` or an alternative Matter stack when a working OnOff bridge sample
exists for this toolchain. The `MatterBackend` trait is the integration boundary.
```

Ensure `main` defaults to `dev` and `RsMatter` selection errors with a helpful message if not compiled in.

- [ ] **Step 3: Verify**

```bash
cd bridge && cargo test && cargo build
./target/debug/thatsmatter-bridge --token test --matter-backend dev
```

If Path A: also document a manual commission smoke checklist in `bridge/README.md` (controller + QR).

- [ ] **Step 4: Commit**

```bash
git add bridge/
git commit -m "feat(bridge): rs-matter spike result and MatterBackend wiring"
```

---

### Task 7: IPC HTTP server (axum) implementing OpenAPI surface

**Files:**
- Create: `bridge/src/ipc/mod.rs`
- Create: `bridge/src/ipc/types.rs`
- Create: `bridge/src/ipc/handlers.rs`
- Create: `bridge/src/ipc/server.rs`
- Create: `bridge/tests/ipc_api.rs`
- Modify: `bridge/src/main.rs` to bind server and run until signal
- Modify: `bridge/src/lib.rs`

**Interfaces:**
- Consumes: OpenAPI paths from Task 1; `AppState` from Task 5.
- Produces: `pub async fn serve(cfg: Config, state: AppState) -> anyhow::Result<()>` binding only loopback.

Auth middleware: every route except `GET /v1/health` requires `X-ThatsMatter-Token` exact match; else `401` with `ErrorBody { error: "unauthorized", message: "..." }`.

Command queue for controller-originated actions (even on Dev backend simulated later):

- Backend or a `CommandQueue` in `AppState` holds pending commands.
- `GET /v1/commands/pending` drains or copies pending list with opaque string ids.
- `POST /v1/commands/{id}/ack` removes id.

For Dev backend in this task, pending commands can stay empty unless a test injects them via an `#[cfg(test)]` helper.

- [ ] **Step 1: Write IPC integration test**

Use `axum::Router` + `tower::ServiceExt` or spawn server on `127.0.0.1:0`.

Cover:

1. Health without token -> 200
2. Status without token -> 401
3. Status with token -> 200, `matter_backend` is `dev`, counts 0
4. PUT catalog with light example -> 200 and non-null `endpoint_id`
5. GET catalog returns same export_id
6. GET pairing returns non-empty manual_code
7. POST fabric/reset without `confirm: true` -> 400
8. POST ha/state with light on -> `{ "applied": 1 }`

- [ ] **Step 2: Implement server and handlers**

Router sketch:

```rust
Router::new()
    .route("/v1/health", get(health))
    .route("/v1/status", get(status))
    .route("/v1/pairing", get(pairing))
    .route("/v1/pairing/open", post(pairing_open))
    .route("/v1/pairing/close", post(pairing_close))
    .route("/v1/fabric/reset", post(fabric_reset))
    .route("/v1/catalog", get(get_catalog).put(put_catalog))
    .route("/v1/ha/state", post(ha_state))
    .route("/v1/commands/pending", get(pending_commands))
    .route("/v1/commands/:id/ack", post(ack_command))
    .layer(middleware::from_fn_with_state(state.clone(), auth_middleware))
```

`put_catalog`: `store.replace` then `backend.apply_catalog`.

`main.rs`: create data_dir, build backend from `cfg.matter_backend`, `serve` forever, handle Ctrl-C.

- [ ] **Step 3: Run tests**

```bash
cd bridge && cargo test --test ipc_api
```

Expected: PASS.

- [ ] **Step 4: Manual smoke**

```bash
cd bridge && cargo build
./target/debug/thatsmatter-bridge --token devtoken --bind 127.0.0.1:18465 --data-dir ./target/data &
curl -s http://127.0.0.1:18465/v1/health
curl -s -H 'X-ThatsMatter-Token: devtoken' http://127.0.0.1:18465/v1/status
kill %1
```

Expected: health `ok`; status JSON with `running: true`.

- [ ] **Step 5: Commit**

```bash
git add bridge/
git commit -m "feat(bridge): loopback HTTP IPC server"
```

---

### Task 8: First-ship device type behavior on the bridge

**Files:**
- Create: `bridge/src/matter/clusters.rs` (mapping tables)
- Modify: `bridge/src/matter/dev.rs` (simulate state)
- Modify: `bridge/src/matter/rs_matter_backend.rs` if Path A
- Create: `bridge/tests/device_types.rs`

**Interfaces:**
- Produces mapping from `DeviceType` to capability set used when applying HA state and encoding commands:

| DeviceType | HA primary domain hints | Matter capabilities (first ship) |
|---|---|---|
| `light` | `light` | on/off + level (brightness 0–255 HA -> 0–254 Matter) |
| `on_off_switch` | `switch`, `input_boolean` | on/off |
| `on_off_plug` | `switch` | on/off |
| `outlet` | `switch` (outlet class) | on/off |
| `contact` | `binary_sensor` | boolean state |
| `motion` | `binary_sensor` | boolean state |
| `cover` | `cover` | position 0–100 and/or open/close/stop |
| `garage` | `cover` | open/close (and position if available) |

Rules:

- Disabled exports: do not accept commands; HA state updates ignored for Matter advertisement (dev backend marks unavailable).
- Unknown entity in state update: skip, do not error whole batch.
- `CommandRequest` kinds only as in schema.

- [ ] **Step 1: Tests for brightness mapping and cover position clamp**

```rust
#[test]
fn brightness_ha_to_matter_level() {
    assert_eq!(ha_brightness_to_matter(0), 0);
    assert_eq!(ha_brightness_to_matter(255), 254);
    assert_eq!(ha_brightness_to_matter(128), 127); // document exact formula in code
}
```

Implement formula: `if bri == 0 { 0 } else { ((bri as u16 * 254) / 255) as u8 }` (adjust only if rs-matter expects different).

- [ ] **Step 2: Implement mapping helpers and wire into `apply_ha_state`**

- [ ] **Step 3: Dev backend command injection helper for tests**

When Dev receives a simulated controller toggle, push `CommandRequest` into pending queue so HA coordinator tests can ack later.

- [ ] **Step 4: `cargo test` and commit**

```bash
cd bridge && cargo test
git add bridge/
git commit -m "feat(bridge): first-ship device type mappings"
```

---

### Task 9: Home Assistant custom component skeleton

**Files:**
- Create: `custom_components/thatsmatter/manifest.json`
- Create: `custom_components/thatsmatter/const.py`
- Create: `custom_components/thatsmatter/__init__.py`
- Create: `custom_components/thatsmatter/strings.json`
- Create: `custom_components/thatsmatter/services.yaml` (empty services ok initially)
- Create: `hacs.json` (optional HACS metadata)

**Interfaces:**
- Produces domain `thatsmatter`, config entry title "ThatsMatter".

`manifest.json`:

```json
{
  "domain": "thatsmatter",
  "name": "ThatsMatter",
  "codeowners": ["@felipefdl"],
  "config_flow": true,
  "dependencies": [],
  "documentation": "https://github.com/felipefdl/thatsmatter",
  "integration_type": "service",
  "iot_class": "local_push",
  "issue_tracker": "https://github.com/felipefdl/thatsmatter/issues",
  "requirements": ["aiohttp"],
  "version": "0.1.0"
}
```

Use a real documentation URL only if the public repo exists; otherwise point to repo-relative docs in README and use a placeholder issue tracker matching the eventual remote. Prefer omitting live URLs that 404; if unknown, set documentation to the product-spec path description in README and use `"documentation": "https://github.com/felipefdl/thatsmatter"` only when that matches intended remote.

`const.py`:

```python
DOMAIN = "thatsmatter"
STORAGE_KEY = "thatsmatter_exports"
STORAGE_VERSION = 1
CONF_BRIDGE_HOST = "bridge_host"
CONF_BRIDGE_PORT = "bridge_port"
CONF_TOKEN = "token"
CONF_BRIDGE_BINARY = "bridge_binary"
CONF_DATA_DIR = "data_dir"
DEFAULT_BRIDGE_HOST = "127.0.0.1"
DEFAULT_BRIDGE_PORT = 18465
DEFAULT_BRIDGE_NAME = "ThatsMatter"
```

`__init__.py`: async setup entry stub that logs and returns True (full wiring in Task 13).

- [ ] **Step 1: Create files above**

- [ ] **Step 2: Verify JSON**

```bash
python3 -c "import json; json.load(open('custom_components/thatsmatter/manifest.json')); json.load(open('custom_components/thatsmatter/strings.json'))"
```

- [ ] **Step 3: Commit**

```bash
git add custom_components/thatsmatter hacs.json 2>/dev/null || git add custom_components/thatsmatter
git commit -m "feat(ha): ThatsMatter custom component skeleton"
```

---

### Task 10: Export models, device type defaults, and HA storage

**Files:**
- Create: `custom_components/thatsmatter/models.py`
- Create: `custom_components/thatsmatter/device_types.py`
- Create: `custom_components/thatsmatter/store.py`
- Create: `tests/component/test_device_types.py`
- Create: `tests/component/test_store.py`
- Create: `tests/component/conftest.py`

**Interfaces:**
- Produces:

```python
@dataclass
class Export:
    export_id: str  # uuid4 str
    name: str
    type: str  # DeviceType value
    primary_entity_id: str
    linked: dict[str, str]
    area_id: str | None
    enabled: bool
    endpoint_id: int | None

    def to_protocol_dict(self) -> dict: ...
    @staticmethod
    def from_protocol_dict(data: dict) -> Export: ...
```

`device_types.py`:

```python
FIRST_SHIP_TYPES = {
    "light",
    "on_off_switch",
    "on_off_plug",
    "outlet",
    "contact",
    "motion",
    "cover",
    "garage",
}

def default_type_for_entity(entity_id: str, device_class: str | None, domain: str) -> str | None:
    """Return default Matter type key or None if unsupported."""
```

Defaults (from product spec):

| Source | Default type |
|---|---|
| domain `light` | `light` |
| domain `switch` + device_class `outlet` | `outlet` |
| domain `switch` or `input_boolean` | `on_off_switch` |
| domain `cover` + device_class in garage/gate | `garage` |
| domain `cover` else | `cover` |
| domain `binary_sensor` + contact/door/window | `contact` |
| domain `binary_sensor` + motion/occupancy | `motion` |
| else | `None` (reject at add) |

`ExportStore` wraps `homeassistant.helpers.storage.Store` with key `STORAGE_KEY`, version `STORAGE_VERSION`, data shape:

```python
{
  "bridge_name": "ThatsMatter",
  "exports": [ ... Export dicts ... ]
}
```

Methods: `async load()`, `async save()`, `get(export_id)`, `upsert(export)`, `delete(export_id)`, `list()`, `set_bridge_name(name)`.

New exports: `export_id = str(uuid.uuid4())`. Never derive from entity_id.

- [ ] **Step 1: Write unit tests that do not need full HA**

Prefer pure functions for `default_type_for_entity` and `Export` round-trip without HA.

For `store.py`, either:

- abstract storage behind a simple protocol with an in-memory fake for unit tests, or
- use HA test harness if already available.

This plan chooses an in-memory fake:

```python
class ExportStore:
    def __init__(self, hass=None, backend: dict | None = None):
        self._data = backend if backend is not None else {"bridge_name": DEFAULT_BRIDGE_NAME, "exports": []}
```

When `hass` is provided, persist via HA Store; when not, use memory (tests).

- [ ] **Step 2: Implement modules**

- [ ] **Step 3: Run**

```bash
python3 -m pytest tests/component/test_device_types.py tests/component/test_store.py -q
```

Expected: PASS.

- [ ] **Step 4: Commit**

```bash
git add custom_components/thatsmatter tests/component
git commit -m "feat(ha): export store and first-ship device type defaults"
```

---

### Task 11: Bridge HTTP client (Python)

**Files:**
- Create: `custom_components/thatsmatter/bridge_client.py`
- Create: `tests/component/test_bridge_client.py`

**Interfaces:**
- Produces:

```python
class BridgeClient:
    def __init__(self, host: str, port: int, token: str, session: aiohttp.ClientSession): ...

    async def health(self) -> dict: ...
    async def status(self) -> dict: ...
    async def pairing(self) -> dict: ...
    async def open_pairing(self) -> dict: ...
    async def close_pairing(self) -> dict: ...
    async def reset_fabric(self) -> dict: ...
    async def get_catalog(self) -> dict: ...
    async def put_catalog(self, snapshot: dict) -> dict: ...
    async def post_ha_state(self, states: list[dict]) -> dict: ...
    async def pending_commands(self) -> list[dict]: ...
    async def ack_command(self, command_id: str, result: dict) -> None: ...
```

Base URL: `http://{host}:{port}`. Header: `X-ThatsMatter-Token: {token}`. Raise `BridgeClientError` with status and body on non-2xx.

- [ ] **Step 1: Write tests with `aiohttp` test server or `aioresponses` / manual mock**

Minimal approach without extra deps: subclass or inject an object with `request()`; or use `aiohttp.web` application in pytest-asyncio.

Add to `requirements-dev.txt` if needed: `pytest-aiohttp`, `pytest-asyncio`.

Test: put_catalog sends JSON body and parses endpoint_id.

- [ ] **Step 2: Implement client**

- [ ] **Step 3: Run pytest and commit**

```bash
python3 -m pytest tests/component/test_bridge_client.py -q
git add custom_components/thatsmatter/bridge_client.py tests/component requirements-dev.txt
git commit -m "feat(ha): bridge IPC HTTP client"
```

---

### Task 12: Config flow and pairing strings

**Files:**
- Create: `custom_components/thatsmatter/config_flow.py`
- Modify: `custom_components/thatsmatter/strings.json`
- Create: `tests/component/test_config_flow.py` (as far as pure validation allows)

**Interfaces:**
- User steps:
  1. `user`: bridge host (default `127.0.0.1`), port (default `18465`), token (required), optional path to bridge binary and data dir if component will spawn process.
  2. On submit: `BridgeClient.health()` then `status()`; abort with `cannot_connect` or `invalid_auth` on failure.
  3. Create entry with title `ThatsMatter`.

Single instance: `async def async_get_options_flow` optional; `CONNECTION_CLASS` local push.

`strings.json` must include user-facing copy from product spec (plain tone):

- Local only. No cloud account for ThatsMatter.
- Nothing is exposed until you add it to the catalog.
- You set the name and type each controller sees.

No em dashes. No certification claims.

- [ ] **Step 1: Implement config flow with mocked client in tests**

- [ ] **Step 2: Manual checklist** (document in PR or component README fragment): install as custom component, add integration, see form.

- [ ] **Step 3: Commit**

```bash
git add custom_components/thatsmatter tests/component
git commit -m "feat(ha): config flow for bridge connection"
```

---

### Task 13: Wiring: start/sync bridge, push catalog, state loop, commands

**Files:**
- Create: `custom_components/thatsmatter/coordinator.py`
- Create: `custom_components/thatsmatter/process.py` (optional subprocess supervisor)
- Modify: `custom_components/thatsmatter/__init__.py`
- Create: `scripts/run_bridge.sh`
- Create: `custom_components/thatsmatter/services.yaml` (reload, open_pairing, reset_fabric with confirm)
- Modify: `bridge` as needed for any ack gaps

**Interfaces:**
- On entry setup:
  1. Load `ExportStore`.
  2. Construct `BridgeClient`.
  3. If configured to manage process: spawn `thatsmatter-bridge` with token file and data dir; wait for health.
  4. `PUT /v1/catalog` full snapshot from store.
  5. Subscribe to HA `state_changed` for all `primary_entity_id` and linked entities in enabled exports.
  6. Debounce and `POST /v1/ha/state`.
  7. Poll `GET /v1/commands/pending` every 0.5s (or long-poll later); for each command call HA services (`light.turn_on`, etc.) then ack.

Service map (command kind -> HA):

| kind | HA service |
|---|---|
| `on_off` on true | `homeassistant.turn_on` / domain turn_on on primary |
| `on_off` on false | domain turn_off |
| `level` | `light.turn_on` brightness |
| `cover_position` | `cover.set_cover_position` |
| `cover_open` | `cover.open_cover` |
| `cover_close` | `cover.close_cover` |
| `cover_stop` | `cover.stop_cover` |

Soft disable: `enabled=false` remains in store and catalog PUT; bridge stops advertising usefulness; HA still keeps config.

- [ ] **Step 1: Implement coordinator methods**

```python
class ThatsMatterCoordinator:
    async def async_start(self) -> None: ...
    async def async_stop(self) -> None: ...
    async def async_push_catalog(self) -> None: ...
    async def async_handle_state_event(self, event) -> None: ...
    async def async_poll_commands(self) -> None: ...
```

- [ ] **Step 2: Wire `__init__.py` setup/unload**

- [ ] **Step 3: `scripts/run_bridge.sh`**

```bash
#!/usr/bin/env bash
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
TOKEN="${THATSMATTER_TOKEN:-devtoken}"
exec cargo run --manifest-path "$ROOT/bridge/Cargo.toml" -- \
  --token "$TOKEN" \
  --bind "127.0.0.1:18465" \
  --data-dir "$ROOT/bridge/target/data" \
  --matter-backend dev
```

- [ ] **Step 4: End-to-end manual verification (dev backend)**

1. Start bridge via script.
2. Load component against token.
3. Add export in store (via service or temporary debug service `thatsmatter.add_export` if UI not ready).
4. Confirm `GET /v1/catalog` on bridge shows export with endpoint_id.
5. Change HA entity state; confirm bridge applied count increments (logs).

If UI not ready (Task 14), temporary services are allowed but must be replaced by catalog UI.

- [ ] **Step 5: Commit**

```bash
git add custom_components/thatsmatter scripts/run_bridge.sh bridge
git commit -m "feat: wire HA coordinator to bridge IPC"
```

---

### Task 14: Catalog UX (HA) - add, list, edit, enable/disable, delete

**Files:**
- Create: `custom_components/thatsmatter/config_flow.py` options / additional flows OR panel via `async_register` websocket + frontend (minimal first: services + repair/issue free form)
- Prefer **first implementable path** inside HA without a custom Lovelace panel:

  1. Config entry options flow: manage bridge name, show pairing code (read-only fields).
  2. Services: `thatsmatter.add_export`, `thatsmatter.update_export`, `thatsmatter.set_enabled`, `thatsmatter.remove_export`, `thatsmatter.reload`.
  3. Diagnostic sensors or logbook notes for status.

- Create: `custom_components/thatsmatter/services.py`
- Modify: `services.yaml`, `strings.json`
- Later enhancement (same plan, separate sub-steps): entity picker UI via config subentries or HA selectors in options flow.

**Interfaces:**
- `add_export` fields: `entity_id` (required), `name` (optional default HA friendly name), `type` (optional default from `device_types`), `enabled` default true.
- Validates unsupported domain with clear error.
- Always creates new `export_id`.
- After any mutation: save store + `async_push_catalog`.

Preview string helper:

```python
def preview_line(export: Export) -> str:
    return f"Controllers will see: {export.name} · {export.type}"
```

Pairing: options flow step `pairing` shows `manual_code` and `qr_payload` from client; note QR image rendering can be text payload first.

- [ ] **Step 1: Implement services + tests for validation**

- [ ] **Step 2: Options flow for pairing material and bridge name**

- [ ] **Step 3: Document user path in `README.md`** (install, run bridge, add integration, add export service call example)

Example service data:

```yaml
service: thatsmatter.add_export
data:
  entity_id: light.kitchen_lamp
  name: Kitchen Lamp
  type: light
```

- [ ] **Step 4: Commit**

```bash
git add custom_components/thatsmatter README.md
git commit -m "feat(ha): catalog services and pairing options flow"
```

---

### Task 15: Integration tests across protocol, bridge, and component client

**Files:**
- Create: `tests/e2e/test_catalog_roundtrip.py`
- Modify: `scripts/check_protocol.sh` or add `scripts/test_all.sh`
- Create: `scripts/test_all.sh`

**Interfaces:**
- Spawns bridge binary with temp data dir and random token.
- Uses `BridgeClient` to PUT catalog_light example, GET back, assert same `export_id` and assigned `endpoint_id`.
- POST HA state; assert applied >= 1.
- Status shows `enabled_export_count == 1`.

- [ ] **Step 1: Write e2e test** (skip if binary missing with clear message)

```python
@pytest.mark.asyncio
async def test_catalog_roundtrip(tmp_path):
    # start subprocess thatsmatter-bridge
    # BridgeClient put/get
    # terminate process
```

- [ ] **Step 2: `scripts/test_all.sh`**

```bash
#!/usr/bin/env bash
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
"$ROOT/scripts/check_protocol.sh"
(cd "$ROOT/bridge" && cargo test)
python3 -m pytest "$ROOT/tests" -q
```

- [ ] **Step 3: Run full suite**

```bash
./scripts/test_all.sh
```

Expected: all green.

- [ ] **Step 4: Commit**

```bash
git add tests scripts
git commit -m "test: e2e catalog roundtrip via IPC"
```

---

### Task 16: Root docs, packaging notes, and success criteria checklist

**Files:**
- Modify: `README.md`
- Create: `bridge/README.md` updates if needed
- Create: `docs/dev-setup.md`
- Create: `docs/network.md` (IPv6, mDNS, Docker host network caveats from product spec)

**Interfaces:**
- README sections: What it is, Status, Architecture diagram (text), Dev setup, Device types, Controller behavior caveats, non-goals.
- Explicit success criteria checklist mirrored from product spec (user can pair when Matter backend is real; with Dev backend, catalog and UX still satisfy "what is exposed?").

Do not claim CSA certification or official HA core status.

- [ ] **Step 1: Write docs**

- [ ] **Step 2: Self-check against product spec**

Map each success criterion:

1. Install and pair -> Path A Matter backend or documented Dev limitation.
2. Add light and switch exports with custom names -> Task 14 services.
3. Only those devices exposed -> empty-by-default + catalog PUT.
4. Disable without delete -> `enabled` flag.
5. Answer "what is exposed?" from catalog -> store list / GET catalog.

- [ ] **Step 3: Commit**

```bash
git add README.md docs bridge/README.md
git commit -m "docs: dev setup, network notes, and success checklist"
```

---

## Self-review (plan author)

### Spec coverage

| Spec area | Tasks |
|---|---|
| Local-only Matter bridge role | 5–8, 13, 16 |
| Opt-in empty catalog | 4, 10, 14 |
| Stable `export_id` | 1, 4, 10 |
| HA config flow + pairing UI material | 12, 14 |
| Export editor fields (name, type, bindings, enable) | 10, 14 |
| First-ship device types | 1, 8, 10 |
| Python component + Rust bridge split | file map, 3–14 |
| IPC contract | 1, 2, 7, 11 |
| rs-matter prefer with real fallback | 5, 6 |
| Network/Docker caveats | 16 |
| Soft disable / controller caveats copy | 14, 16 |
| No separate admin website | 12–14 (HA only) |
| Implementation order (spike -> dynamic -> component -> packaging) | 6 then 7–8 then 9–14; packaging notes in 16 (HAOS add-on deferred as follow-up, not blocking) |

Gaps intentionally deferred (called out, not silent):

- Full Lovelace catalog panel (services + options flow first).
- HAOS add-on packaging (document only in Task 16).
- Temperature, humidity, thermostat, fan, lock (out of first ship).
- Unix socket transport (documented future only).
- Color cluster for lights beyond on/off + brightness.

### Placeholder scan

No TBD steps. Each task names files, commands, and expected results. Task 6 has explicit Path A / Path B, not an open TBD.

### Type consistency

Protocol `DeviceType` string enums match Rust `DeviceType` serde renames and Python `FIRST_SHIP_TYPES`. Export fields: `export_id`, `name`, `type`, `primary_entity_id`, `linked`, `area_id`, `enabled`, `endpoint_id` identical across schema, Rust, Python. Header token name: `X-ThatsMatter-Token`. Default port: `18465`. Domain: `thatsmatter`.

---

## Execution handoff

Plan complete and saved to `docs/superpowers/plans/2026-07-31-thatsmatter.md`.

Recommended execution: **superpowers:subagent-driven-development** (fresh subagent per task, review between tasks). Alternative: **superpowers:executing-plans** inline with checkpoints.

Start at Task 1 (protocol). Do not skip Task 6's real rs-matter attempt.
