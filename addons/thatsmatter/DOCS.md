# ThatsMatter App

Matter bridge for Home Assistant OS. Image: `ghcr.io/felipefdl/thatsmatter`.

## Install

1. Add repository `https://github.com/felipefdl/thatsmatter` in the Add-on store
2. Install **ThatsMatter** → Start
3. Install the **ThatsMatter** integration via [HACS](https://hacs.xyz) (custom repository, same GitHub URL) or copy `custom_components/thatsmatter`
4. Settings → Devices & services → Add **ThatsMatter**

Full guide: https://github.com/felipefdl/thatsmatter/blob/main/docs/haos-install.md

## Options

| Option | Default | Notes |
|---|---|---|
| Bridge name | ThatsMatter | Matter node name |
| Listen port | 18465 | Integration IPC |
| Matter backend | rs_matter | Use `dev` only for offline IPC tests |
| Log level | info | Bridge logs |
| LAN interface | _(empty)_ | Host interface name (`eth0`, `enp1s0`, `wlan0`) that Matter **reports/requires as operational** for the stack netif view. Empty = auto-select the best non-virtual face **with IPv6** (link-local OK; skips Docker/hassio). A wrong pin fails start with available names. Does **not** force Avahi to a single NIC — Avahi multi-homes per host Avahi policy unless you configure Avahi separately. |

## Pairing and devices

Use the integration **Configure** menu (not YAML):

- **Add devices to export**
- **Pair with other apps** (opens the pairing window and shows setup code + QR on the device page)

### Multi-NIC / HomeKit tips

1. **Stop Matterbridge** (or any other Matter accessory on UDP **5540**) before starting/pairing ThatsMatter on the same host — the bridge preflights 5540 and will refuse to start if the port is taken.
2. Controllers (phone, hub, HA Matter Server) must share the **same L2 LAN** as the selected interface. Matter needs **IPv6 on that face** (link-local `fe80::` is enough).
3. Prefer a working **Avahi** daemon on the host (this App has `host_dbus`). The bridge probes Avahi over D-Bus before using it; if the probe fails it falls back to Zeroconf — on Linux/HAOS both paths typically still need the Avahi daemon.
4. If controllers never see the bridge, set **LAN interface** to your real Ethernet/Wi‑Fi name and **restart** the App. Check logs for `LAN netif inventory` and `Matter LAN interface …`. That option only steers the Matter stack's netif view; Avahi advertisement multi-homing is still host Avahi policy.
5. Do **not** keep re-opening the pairing window while it is already open — the bridge treats that as a true no-op (deadline unchanged). If another admin window is active (e.g. controller ECM), open returns an error instead of closing that foreign window.

### Multi-admin pairing

1. On first start with no fabrics, the bridge opens a basic commissioning window for 15 minutes. Pair the first app (Alexa, Google, Apple, SmartThings, or Home Assistant Matter) with the setup code or QR while that window is open.
2. After the first fabric is added, the window closes. To add another app, press **Open pairing window** on the ThatsMatter device in Home Assistant, or open **Configure → Pair with other apps** again. Pair the next app within the window (3–15 minutes depending on the timeout).
3. Controllers that already own a fabric can also open a window for a new admin via Home Assistant's Matter **share device** flow (AdministratorCommissioning / enhanced window). That controller-driven window is not reflected in ThatsMatter's `pairing_open` status; use the bridge button or Configure step when you want the printed setup code to work.

The setup code sensor and pairing QR image are only available while the bridge-tracked window is open.
