# ThatsMatter bridge

Rust process: Matter node + loopback HTTP JSON control plane for the Home Assistant custom component.

## Backends

| `--matter-backend` | Behavior |
|---|---|
| `rs_matter` (default) | `rs-matter` + `rs-matter-stack` Ethernet bridge; real pairing codes; Avahi/Zeroconf mDNS |
| `dev` | Offline IPC only; same pairing code algorithm; no network advertise |

The setup passcode and discriminator are random per install, generated on first start and stored in `<data-dir>/commissioning.json`. Device attestation still uses CSA **test** credentials, so controllers may show uncertified prompts.

## Endpoint model

- Catalog is opt-in and may hold many exports.
- The Matter fabric is a **bridge**: endpoint 0 root, endpoint 1 aggregator, then one bridged endpoint per enabled export at catalog `endpoint_id` + 1 (OnOff, cover/garage, contact, motion).
- Optional `--mdns-interface` / `THATSMATTER_MDNS_INTERFACE` pins which LAN face the stack treats as operational (must be up with IPv6). Empty auto-selects a non-virtual face with IPv6.

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
