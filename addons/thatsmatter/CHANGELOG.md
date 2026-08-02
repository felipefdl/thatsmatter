# Changelog

## 0.2.4

- Builtin mDNS binds UDP 5353 with SO_REUSEADDR/SO_REUSEPORT so it can share the port with Home Assistant Core (was Address already in use)
- Increase Matter stack bump arena to avoid panic after mDNS bind retries

## 0.2.3

- When host Avahi is missing (typical HAOS), use built-in mDNS (UDP 5353) instead of Zeroconf, which cannot advertise without Avahi

## 0.2.2

- Fix App crash on HAOS: build the bridge on Debian bookworm so glibc matches the runtime image (GLIBC_2.39 not found)

## 0.2.1

- Select a real LAN interface with IPv6 for Matter (skip Docker/hassio virtual faces)
- Prefer system Avahi mDNS for commissionable advertisement; Zeroconf only as fallback
- Optional App option **LAN interface** (`mdns_interface`) to pin the operational face
- Fail start clearly when UDP 5540 is already in use (e.g. Matterbridge running)
- Skip pairing-window close/reopen when the window is already open
- `GET /health` reports `ok` only while the Matter backend is running
- Docs: multi-endpoint bridge, IPv6 link-local, stop Matterbridge before pairing

## 0.2.0

- Bridged multi-endpoint Matter node with live HA state subscriptions
- Cover, garage, contact, and motion endpoints in addition to OnOff
- Per-install pairing material (passcode/discriminator stored under data dir)
- Pairing window open/close over IPC, HA button, and options flow
- Unregister services on unload; push entity updates without polling
- Ruff wired into ha-lint and CI

## 0.1.2

- Align bridge, App, and integration versions
- Catalog and state resync on bridge reconnect
- Export validation and service field fixes
- Bridge on/off from primary entity only; PATCH null clears area_id

## 0.1.0

- Initial App and integration release
