# HA-as-Matter-controller loop

ThatsMatter is a Matter **bridge** (device side). Home Assistant’s Matter integration is the **controller** (client). That closed loop is the acceptance path; Alexa/Google/Apple are optional after it works.

## Topology

```text
demo light (HA entity)
        │
        ▼
ThatsMatter custom_component ──HTTP──► thatsmatter-bridge (rs_matter)
                                              │
                                              │ Matter IP + mDNS
                                              ▼
                                    python-matter-server
                                              │
                                              ▼
                                    HA Matter integration
                                    (matter.* entities)
```

## Prerequisites

- Docker + Docker Compose
- Linux host preferred (`network_mode: host` for mDNS)
- On Docker Desktop (macOS), host networking is limited: use the loop for IPC verification and capture mDNS failure honestly if commission fails

## Start services

```bash
# Bridge only (IPC + Matter stack on host network)
docker compose up --build thatsmatter-bridge

# Full loop (Linux)
docker compose --profile ha --profile matter up --build
```

HA UI: `http://localhost:8123`  
Bridge IPC (host network): `http://127.0.0.1:18465`

## Procedure

1. Complete HA onboarding (if first run).
2. Confirm **ThatsMatter** is available under Settings → Devices & services (custom component is bind-mounted).
3. Add ThatsMatter with:
   - host `127.0.0.1`
   - port `18465`
4. Add Matter integration; point it at the Matter Server (`ws://127.0.0.1:5580/ws` or the default add-on path).
5. In HA: **Settings → Devices & services → ThatsMatter → Configure → Add devices to export** and pick a demo light.
6. **Configure → Pair with other apps** for the setup code, or open the **Setup code** / **Pairing QR code** on the ThatsMatter device page.
7. In Matter integration (or another controller), commission with that setup code / QR.
8. Toggle the resulting `matter.*` light; the demo light should change (controller → bridge → HA).
9. Toggle the demo light; Matter state should follow (HA → bridge → Matter).

## Local (no Docker) smoke

```bash
# Offline IPC (dev backend)
just smoke

# Commissionable backend start + pairing material
bash scripts/smoke_rs_matter.sh
```

## Automated commission attempt

```bash
# Evidence directory required
bash scripts/ha_loop.sh /path/to/evidence 1
bash scripts/ha_loop.sh /path/to/evidence 2
# or
just ha-loop OUT=/path/to/evidence
```

`scripts/ha_loop.sh` starts `docker compose --profile ha --profile matter`, waits for bridge + HA, then calls Matter Server WebSocket `commission_with_code` (same API HA’s Matter integration uses) via `scripts/ha_loop_commission.py`.

Exit `0` = commission OK. Exit `2` = environment / discovery / commission failure (logs are the proof).

## Known limits

- One Matter OnOff endpoint is bound to the **primary** enabled OnOff export (lowest `endpoint_id`). Catalog may hold more exports; only the primary is on the fabric until multi-endpoint bridge work lands.
- The setup passcode and discriminator are random per install, stored in `<data-dir>/commissioning.json`. Attestation still uses the CSA **test** VID/PID, so controllers may show “uncertified”.
- Docker Desktop on macOS: containers may start and bridge IPC works, but Matter mDNS discovery between Matter Server and the bridge often fails (`commission_with_code` error_code=1). That is an environment failure of the full loop, not a silent skip.
