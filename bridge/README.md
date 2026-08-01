# ThatsMatter bridge

Rust process: Matter node + loopback HTTP JSON control plane for the Home Assistant custom component.

## Backends

| `--matter-backend` | Behavior |
|---|---|
| `rs_matter` (default) | `rs-matter` + `rs-matter-stack` Ethernet OnOff device; real pairing codes; mDNS |
| `dev` | Offline IPC only; same pairing code algorithm; no network advertise |

Pairing uses CSA **test** device constants (passcode `20202021`, discriminator `3840`). Controllers may show uncertified prompts.

## Endpoint model

- Catalog is opt-in and may hold many exports.
- The Matter fabric currently exposes **one** OnOff light endpoint (id `1`) bound to the primary enabled OnOff export (lowest `endpoint_id`).

## Run

```bash
cargo run --manifest-path bridge/Cargo.toml -- \
  --listen 127.0.0.1:18465 \
  --data-dir ./data \
  --matter-backend rs_matter
```

Docker / LAN bind:

```bash
./thatsmatter-bridge --listen 0.0.0.0:18465 --allow-non-loopback --matter-backend rs_matter
```

## Tests

```bash
cargo test --manifest-path bridge/Cargo.toml
bash scripts/smoke_ipc.sh
bash scripts/smoke_rs_matter.sh
```
