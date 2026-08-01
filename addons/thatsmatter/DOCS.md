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
| LAN interface | _(empty)_ | Optional host interface name (`eth0`, `enp1s0`, `wlan0`). Empty = auto-select like Matterbridge (skips Docker/hassio faces). Set when multiple NICs break HomeKit/controller discovery. |

## Pairing and devices

Use the integration **Configure** menu (not YAML):

- **Add devices to export**
- **Pair with other apps** (opens the pairing window and shows setup code + QR on the device page)

### Multi-NIC / HomeKit tips

1. **Stop Matterbridge** (or any other Matter accessory on UDP **5540**) before pairing ThatsMatter on the same host.
2. Prefer **Avahi** on the host (this App has `host_dbus` and uses system Avahi when available).
3. If controllers never see the bridge, set **LAN interface** to your real Ethernet/Wi‑Fi name and **restart** the App. Check logs for `LAN netif inventory` / `selected Matter LAN interface`.
4. Do **not** keep re-opening the pairing window while it is already open — the bridge treats that as a no-op so the stack window is not thrashed.

### Multi-admin pairing

1. On first start with no fabrics, the bridge opens a basic commissioning window for 15 minutes. Pair the first app (Alexa, Google, Apple, SmartThings, or Home Assistant Matter) with the setup code or QR while that window is open.
2. After the first fabric is added, the window closes. To add another app, press **Open pairing window** on the ThatsMatter device in Home Assistant, or open **Configure → Pair with other apps** again. Pair the next app within the window (3–15 minutes depending on the timeout).
3. Controllers that already own a fabric can also open a window for a new admin via Home Assistant's Matter **share device** flow (AdministratorCommissioning / enhanced window). That controller-driven window is not reflected in ThatsMatter's `pairing_open` status; use the bridge button or Configure step when you want the printed setup code to work.

The setup code sensor and pairing QR image are only available while the bridge-tracked window is open.
