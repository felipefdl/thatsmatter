#!/usr/bin/env python3
"""Commission ThatsMatter into python-matter-server (HA Matter controller backend).

Uses the same WebSocket API Home Assistant's Matter integration uses.
Exit 0 on successful commission; exit 2 on controller/network failure; exit 1 on script errors.
"""

from __future__ import annotations

import argparse
import asyncio
import json
import sys
import time
import urllib.error
import urllib.request
from typing import Any

try:
  import websockets
except ImportError:
  print("websockets package required: pip install websockets", file=sys.stderr)
  sys.exit(1)


def http_get_json(url: str, timeout: float = 5.0) -> dict[str, Any]:
  req = urllib.request.Request(url, headers={"Accept": "application/json"})
  with urllib.request.urlopen(req, timeout=timeout) as resp:
    return json.loads(resp.read().decode())


def wait_http(url: str, timeout_s: float, label: str) -> None:
  deadline = time.time() + timeout_s
  last_err = ""
  while time.time() < deadline:
    try:
      with urllib.request.urlopen(url, timeout=3) as resp:
        if resp.status < 500:
          print(f"ready: {label} ({url})")
          return
    except Exception as exc:  # noqa: BLE001
      last_err = str(exc)
    time.sleep(1.0)
  raise RuntimeError(f"timeout waiting for {label} at {url}: {last_err}")


async def commission(ws_url: str, code: str, network_only: bool, out_path: str | None) -> int:
  print(f"connecting matter-server {ws_url}")
  async with websockets.connect(ws_url, max_size=None, open_timeout=30) as ws:
    # First message is server info
    raw = await asyncio.wait_for(ws.recv(), timeout=30)
    info = json.loads(raw)
    print("server_info:", json.dumps(info)[:500])
    if out_path:
      with open(out_path.replace(".json", "-server-info.json"), "w", encoding="utf-8") as f:
        json.dump(info, f, indent=2)

    msg_id = "1"
    payload = {
      "message_id": msg_id,
      "command": "commission_with_code",
      "args": {
        "code": code,
        "network_only": network_only,
      },
    }
    print("send:", json.dumps(payload))
    await ws.send(json.dumps(payload))

    # Wait for result matching message_id (events may interleave)
    deadline = time.time() + 120
    while time.time() < deadline:
      raw = await asyncio.wait_for(ws.recv(), timeout=120)
      data = json.loads(raw)
      print("recv:", json.dumps(data)[:800])
      if data.get("message_id") != msg_id:
        continue
      if out_path:
        with open(out_path, "w", encoding="utf-8") as f:
          json.dump(data, f, indent=2)
      if "error_code" in data:
        print(
          f"COMMISSION_FAILED error_code={data.get('error_code')} details={data.get('details')}",
          file=sys.stderr,
        )
        return 2
      if "result" in data:
        print("COMMISSION_OK")
        return 0
    print("COMMISSION_TIMEOUT", file=sys.stderr)
    return 2


def main() -> int:
  parser = argparse.ArgumentParser(description=__doc__)
  parser.add_argument("--bridge-url", default="http://127.0.0.1:18465")
  parser.add_argument("--matter-ws", default="ws://127.0.0.1:5580/ws")
  parser.add_argument("--ha-url", default="http://127.0.0.1:8123")
  parser.add_argument("--code", default="", help="Override pairing code/QR; default from bridge /pairing")
  parser.add_argument("--network-only", action="store_true", default=True)
  parser.add_argument("--no-network-only", action="store_true")
  parser.add_argument("--out", default="", help="Write commission response JSON")
  parser.add_argument("--skip-ha-wait", action="store_true")
  args = parser.parse_args()

  network_only = not args.no_network_only

  try:
    wait_http(f"{args.bridge_url}/health", 90, "thatsmatter-bridge")
  except RuntimeError as err:
    print(f"ENV_FAILURE bridge: {err}", file=sys.stderr)
    return 2

  try:
    # Matter server has no HTTP health; TCP connect via WS later. Probe HA if requested.
    if not args.skip_ha_wait:
      wait_http(args.ha_url, 180, "homeassistant")
  except RuntimeError as err:
    print(f"ENV_FAILURE homeassistant: {err}", file=sys.stderr)
    return 2

  code = args.code
  if not code:
    pairing = http_get_json(f"{args.bridge_url}/pairing")
    # Prefer QR payload (MT:...) then manual setup code
    code = pairing.get("qr_payload") or pairing.get("setup_code") or ""
    print("pairing from bridge:", json.dumps(pairing))
    if not code:
      print("ENV_FAILURE: empty pairing material from bridge", file=sys.stderr)
      return 2

  # Ensure an OnOff export exists so commission has a useful endpoint
  try:
    exports = http_get_json(f"{args.bridge_url}/exports")
    if not exports:
      body = json.dumps(
        {
          "export_id": "cccccccc-cccc-4ccc-8ddd-eeeeeeeeeeee",
          "name": "Loop Lamp",
          "type": "light",
          "primary_entity_id": "light.bed_light",
          "linked": {},
          "enabled": True,
        }
      ).encode()
      req = urllib.request.Request(
        f"{args.bridge_url}/exports",
        data=body,
        headers={"content-type": "application/json"},
        method="POST",
      )
      with urllib.request.urlopen(req, timeout=10) as resp:
        print("created export:", resp.read().decode()[:300])
  except Exception as exc:  # noqa: BLE001
    print(f"warn: export seed failed: {exc}")

  try:
    return asyncio.run(
      commission(args.matter_ws, code, network_only, args.out or None)
    )
  except Exception as exc:  # noqa: BLE001
    print(f"ENV_FAILURE matter-server commission: {exc}", file=sys.stderr)
    return 2


if __name__ == "__main__":
  sys.exit(main())
