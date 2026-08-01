# ThatsMatter

Expose Home Assistant devices to Matter controllers (Alexa, Google Home, SmartThings, Apple Home, and others) as a local Matter bridge.

Nothing leaves Home Assistant until you add it in the UI. You control the name, type, and which entities each controller sees.

## Install on Home Assistant OS

### 1. App (bridge)

**Settings → System → Apps → Add-on store → ⋮ → Repositories**

```text
https://github.com/felipefdl/thatsmatter
```

Install **ThatsMatter** → **Start**.  
Pulls `ghcr.io/felipefdl/thatsmatter` (amd64 / arm64). No local Rust build.

### 2. Integration (HACS)

**HACS → Integrations → ⋮ → Custom repositories**

| Field | Value |
|---|---|
| Repository | `https://github.com/felipefdl/thatsmatter` |
| Category | Integration |

Download **ThatsMatter** → restart Home Assistant → **Settings → Devices & services → Add integration → ThatsMatter**  
(Host `127.0.0.1`, port `18465` if not auto-discovered.)

### 3. Use

1. **ThatsMatter → Configure → Add devices to export** (entity picker)
2. **Configure → Pair with other apps** (opens the pairing window and shows the setup code; QR on the device page)
3. In Alexa / Google / Apple / SmartThings: **Add device → Matter**
4. For another ecosystem after the first commission: press **Open pairing window** on the ThatsMatter device (or open **Pair with other apps** again), then pair within the window. Apps that already paired can also share the bridge via Home Assistant's Matter **share device** flow.

Full guide: [docs/haos-install.md](docs/haos-install.md)

## Architecture

```text
HA entities
    │  Configure UI (export catalog)
    ▼
custom_components/thatsmatter
    │  HTTP 127.0.0.1:18465
    ▼
ThatsMatter App  (ghcr.io/felipefdl/thatsmatter)
    │  Matter + mDNS
    ▼
Alexa / Google / SmartThings / Apple Home
```

## Container image

| Item | Value |
|---|---|
| Image | `ghcr.io/felipefdl/thatsmatter` |
| Tags | `latest` / `main` on main branch; `0.x.y` on release tags `v0.x.y` |
| Platforms | `linux/amd64`, `linux/arm64` |

```bash
docker pull ghcr.io/felipefdl/thatsmatter:latest
```

## Development

```bash
just verify
cargo run --manifest-path bridge/Cargo.toml -- --matter-backend rs_matter
docker compose --profile ha --profile matter up --build
```

| Path | Role |
|---|---|
| `bridge/` | Rust Matter bridge + IPC |
| `custom_components/thatsmatter/` | HA integration (HACS) |
| `addons/thatsmatter/` | HAOS App definition |
| `protocol/schema.json` | IPC contract |

## Docs

- [HAOS / HACS install](docs/haos-install.md)
- [Product spec](docs/product-spec.md)
- [Docker HA loop](docs/docker-loop.md)
- [Bridge](bridge/README.md)

## License

MIT. See [LICENSE](LICENSE).
