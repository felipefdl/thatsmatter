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

## Pairing and devices

Use the integration **Configure** menu (not YAML):

- **Add devices to export**
- **Pair with other apps** (opens the pairing window and shows setup code + QR on the device page)

### Multi-admin pairing

1. On first start with no fabrics, the bridge opens a basic commissioning window for 15 minutes. Pair the first app (Alexa, Google, Apple, SmartThings, or Home Assistant Matter) with the setup code or QR while that window is open.
2. After the first fabric is added, the window closes. To add another app, press **Open pairing window** on the ThatsMatter device in Home Assistant, or open **Configure → Pair with other apps** again. Pair the next app within the window (3–15 minutes depending on the timeout).
3. Controllers that already own a fabric can also open a window for a new admin via Home Assistant's Matter **share device** flow (AdministratorCommissioning / enhanced window). That controller-driven window is not reflected in ThatsMatter's `pairing_open` status; use the bridge button or Configure step when you want the printed setup code to work.

The setup code sensor and pairing QR image are only available while the bridge-tracked window is open.
