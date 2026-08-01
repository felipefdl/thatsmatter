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
- **Pair with other apps** (setup code + QR on the device page)
