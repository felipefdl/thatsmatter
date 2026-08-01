#!/usr/bin/env bash
# End-to-end IPC smoke against a live bridge process (no Home Assistant).
# Starts the bridge, creates an export, pushes state, checks status/pairing, deletes the export.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BRIDGE_MANIFEST="${ROOT}/bridge/Cargo.toml"
PORT="${SMOKE_PORT:-18466}"
LISTEN="127.0.0.1:${PORT}"
BASE="http://${LISTEN}"
DATA_DIR="${SMOKE_DATA_DIR:-$(mktemp -d -t thatsmatter-smoke.XXXXXX)}"
BRIDGE_PID=""
KEEP_DATA="${SMOKE_KEEP_DATA:-0}"

cleanup() {
  local code=$?
  if [[ -n "${BRIDGE_PID}" ]] && kill -0 "${BRIDGE_PID}" 2>/dev/null; then
    kill "${BRIDGE_PID}" 2>/dev/null || true
    wait "${BRIDGE_PID}" 2>/dev/null || true
  fi
  if [[ "${KEEP_DATA}" != "1" && -d "${DATA_DIR}" ]]; then
    rm -rf "${DATA_DIR}"
  fi
  exit "${code}"
}
trap cleanup EXIT INT TERM

need() {
  command -v "$1" >/dev/null 2>&1 || {
    echo "error: required command not found: $1" >&2
    exit 1
  }
}

need cargo
need curl
need python3

echo "==> smoke_ipc: build bridge"
cargo build --manifest-path "${BRIDGE_MANIFEST}" --quiet

BIN="${ROOT}/bridge/target/debug/thatsmatter-bridge"
if [[ ! -x "${BIN}" ]]; then
  echo "error: bridge binary missing at ${BIN}" >&2
  exit 1
fi

mkdir -p "${DATA_DIR}"
echo "==> smoke_ipc: start bridge on ${LISTEN} (data-dir=${DATA_DIR})"
"${BIN}" --listen "${LISTEN}" --data-dir "${DATA_DIR}" --bridge-name SmokeBridge --matter-backend dev \
  >"${DATA_DIR}/bridge.log" 2>&1 &
BRIDGE_PID=$!

# Wait for /health
for _ in $(seq 1 50); do
  if curl -sf "${BASE}/health" >/dev/null 2>&1; then
    break
  fi
  if ! kill -0 "${BRIDGE_PID}" 2>/dev/null; then
    echo "error: bridge exited early; log:" >&2
    cat "${DATA_DIR}/bridge.log" >&2 || true
    exit 1
  fi
  sleep 0.1
done

if ! curl -sf "${BASE}/health" >/dev/null 2>&1; then
  echo "error: bridge did not become healthy; log:" >&2
  cat "${DATA_DIR}/bridge.log" >&2 || true
  exit 1
fi

json_get() {
  # usage: json_get <url> [jq filter]
  local url="$1"
  local filter="${2:-.}"
  curl -sf "${url}" | python3 -c "
import json, sys
data = json.load(sys.stdin)
filt = sys.argv[1]
if filt == '.':
    print(json.dumps(data, indent=2))
else:
    # minimal dotted path: a.b[0].c style not needed; support simple keys and list index
    cur = data
    for part in filt.split('.'):
        if not part:
            continue
        if part.isdigit():
            cur = cur[int(part)]
        else:
            cur = cur[part]
    if isinstance(cur, (dict, list)):
        print(json.dumps(cur))
    else:
        print(cur)
" "${filter}"
}

json_post() {
  local method="$1"
  local url="$2"
  local body="$3"
  curl -sf -X "${method}" -H "content-type: application/json" -d "${body}" "${url}"
}

echo "==> GET /health"
HEALTH="$(curl -sf "${BASE}/health")"
echo "${HEALTH}" | python3 -c 'import json,sys; d=json.load(sys.stdin); assert d["ok"] is True and d["version"]; print("    ok version=", d["version"])'

echo "==> GET /status"
STATUS="$(curl -sf "${BASE}/status")"
echo "${STATUS}" | python3 -c '
import json,sys
d=json.load(sys.stdin)
assert d["bridge_name"] == "SmokeBridge"
assert d["running"] is True
assert d["matter_backend"] == "dev"
assert d["export_count"] == 0
print("    bridge_name=", d["bridge_name"], "backend=", d["matter_backend"])
'

echo "==> GET /pairing"
PAIRING="$(curl -sf "${BASE}/pairing")"
echo "${PAIRING}" | python3 -c '
import json,sys
d=json.load(sys.stdin)
assert d["setup_code"]
assert d["qr_payload"]
assert "discriminator" in d and "passcode" in d
print("    setup_code=", d["setup_code"][:20] + "...")
'

EXPORT_ID="aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee"
CREATE_BODY=$(python3 -c "
import json
print(json.dumps({
  'export_id': '${EXPORT_ID}',
  'name': 'Smoke Lamp',
  'type': 'light',
  'primary_entity_id': 'light.smoke',
  'linked': {},
  'enabled': True,
}))
")

echo "==> POST /exports"
CREATED="$(json_post POST "${BASE}/exports" "${CREATE_BODY}")"
echo "${CREATED}" | python3 -c '
import json,sys
d=json.load(sys.stdin)
assert d["export_id"] == "'"${EXPORT_ID}"'"
assert d["name"] == "Smoke Lamp"
assert d["type"] == "light"
assert d["endpoint_id"] is not None and d["endpoint_id"] >= 1
print("    export_id=", d["export_id"], "endpoint_id=", d["endpoint_id"])
'

echo "==> POST /exports/{id}/state"
STATE_BODY='{"entity_id":"light.smoke","state":"on","attributes":{"brightness":200}}'
APPLIED="$(json_post POST "${BASE}/exports/${EXPORT_ID}/state" "${STATE_BODY}")"
echo "${APPLIED}" | python3 -c '
import json,sys
d=json.load(sys.stdin)
assert d["applied"] == 1
print("    applied=", d["applied"])
'

echo "==> GET /status (after create)"
curl -sf "${BASE}/status" | python3 -c '
import json,sys
d=json.load(sys.stdin)
assert d["export_count"] == 1
assert d["enabled_export_count"] == 1
print("    export_count=", d["export_count"])
'

echo "==> GET /exports"
curl -sf "${BASE}/exports" | python3 -c '
import json,sys
d=json.load(sys.stdin)
assert isinstance(d, list) and len(d) == 1
print("    list_len=", len(d))
'

echo "==> DELETE /exports/{id}"
DELETED="$(curl -sf -X DELETE "${BASE}/exports/${EXPORT_ID}")"
echo "${DELETED}" | python3 -c '
import json,sys
d=json.load(sys.stdin)
assert d["export_id"] == "'"${EXPORT_ID}"'"
print("    deleted name=", d["name"])
'

echo "==> GET /exports (empty)"
curl -sf "${BASE}/exports" | python3 -c '
import json,sys
d=json.load(sys.stdin)
assert d == []
print("    list_len=0")
'

echo "==> smoke_ipc: PASS"
