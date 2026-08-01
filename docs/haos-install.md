# Install ThatsMatter on Home Assistant OS

Two pieces:

1. **App** — Matter bridge (`ghcr.io/felipefdl/thatsmatter`)
2. **Integration** — catalog UI, pairing, services (`custom_components/thatsmatter`)

## A. App (Add-on store + GHCR)

1. **Settings → System → Apps → Add-on store → ⋮ → Repositories**
2. Add:

   ```text
   https://github.com/felipefdl/thatsmatter
   ```

3. Refresh the store, open **ThatsMatter**, **Install**, **Start**
4. Confirm logs show the bridge listening and Matter started

The App uses **host network** and pulls a multi-arch image from GHCR (amd64 / arm64). No local Rust compile on the Pi.

Image: `ghcr.io/felipefdl/thatsmatter:<version>` (matches add-on `version` in `addons/thatsmatter/config.yaml`).

## B. Integration (HACS)

1. **HACS → Integrations → ⋮ → Custom repositories**
2. Repository: `https://github.com/felipefdl/thatsmatter`
3. Category: **Integration**
4. **Download** ThatsMatter → **Restart** Home Assistant
5. **Settings → Devices & services → Add integration → ThatsMatter**
   - Discovery may appear when the App is running
   - Or enter host `127.0.0.1`, port `18465`

## C. Manual integration (no HACS)

```bash
# On HAOS (SSH / Terminal)
cd /config/custom_components
wget -qO- https://github.com/felipefdl/thatsmatter/archive/refs/heads/main.tar.gz \
  | tar -xz --strip-components=2 thatsmatter-main/custom_components/thatsmatter
```

Restart Home Assistant, then add the integration as above.

## Use (UI only)

1. **Settings → Devices & services → ThatsMatter → Configure**
2. **Add devices to export** — pick entities (no YAML)
3. **Pair with other apps** — setup code; also notification + **Setup code** / **Pairing QR** on the device page
4. In Alexa / Google / SmartThings / Apple Home: **Add device → Matter** → code or QR

## Optional: SSH bundle

```bash
# From a clone of this repo
./scripts/package-haos.sh
HA_SSH=root@homeassistant.local ./scripts/install-haos.sh
```

## Verify

| Check | Expected |
|---|---|
| App status | Running |
| Integration | Bridge connected |
| Configure → Pair | Setup code shown |
| Configure → Add devices | Export count increases |

## Uninstall

1. Remove the ThatsMatter integration  
2. Stop and uninstall the ThatsMatter App  
3. Remove HACS download or `/config/custom_components/thatsmatter` if manual  

## Notes

- Pairing uses CSA **test** credentials (uncertified prompts are normal)
- One Matter OnOff endpoint is bound to the primary enabled OnOff export
- Controllers must share the **same L2 LAN** with HA (mDNS / IPv6). Matter needs **IPv6 on the LAN face** (link-local `fe80::` is enough).
- On multi-NIC hosts, set App option **LAN interface** (e.g. `eth0`) if HomeKit or other controllers cannot find the bridge. That option selects which interface the Matter stack **reports/requires as operational**; empty auto-selects the best non-virtual face with IPv6 (skips Docker/hassio). Avahi still multi-homes per host Avahi policy unless you configure Avahi separately.
- Stop Matterbridge (or any other Matter process) before testing — UDP **5540** must be free; the bridge fails start if the port is in use.
