#!/usr/bin/env bash
# Start bridge with the real rs_matter backend and capture pairing material.
# Does not require HA; proves commissionable pairing codes and stack start.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BRIDGE_MANIFEST="${ROOT}/bridge/Cargo.toml"
PORT="${SMOKE_PORT:-18467}"
LISTEN="127.0.0.1:${PORT}"
BASE="http://${LISTEN}"
DATA_DIR="${SMOKE_DATA_DIR:-$(mktemp -d -t thatsmatter-rsm.XXXXXX)}"
OUT_DIR="${SMOKE_OUT_DIR:-${DATA_DIR}}"
BRIDGE_PID=""

cleanup() {
  local code=$?
  stop_bridge
  exit "${code}"
}
trap cleanup EXIT INT TERM

need() {
  command -v "$1" >/dev/null 2>&1 || {
    echo "error: required command not found: $1" >&2
    exit 1
  }
}

stop_bridge() {
  if [[ -n "${BRIDGE_PID}" ]] && kill -0 "${BRIDGE_PID}" 2>/dev/null; then
    kill "${BRIDGE_PID}" 2>/dev/null || true
    wait "${BRIDGE_PID}" 2>/dev/null || true
  fi
  BRIDGE_PID=""
}

start_bridge() {
  local log="$1"
  "${BIN}" \
    --listen "${LISTEN}" \
    --data-dir "${DATA_DIR}" \
    --bridge-name ThatsMatter \
    --matter-backend rs_matter \
    >"${log}" 2>&1 &
  BRIDGE_PID=$!

  for _ in $(seq 1 100); do
    if curl -sf "${BASE}/health" >/dev/null 2>&1; then
      return 0
    fi
    if ! kill -0 "${BRIDGE_PID}" 2>/dev/null; then
      echo "error: bridge exited early; log:" >&2
      cat "${log}" >&2 || true
      exit 1
    fi
    sleep 0.2
  done

  echo "error: bridge did not become healthy; log:" >&2
  cat "${log}" >&2 || true
  exit 1
}

need cargo
need curl
need python3

mkdir -p "${OUT_DIR}" "${DATA_DIR}"

echo "==> smoke_rs_matter: build bridge"
cargo build --manifest-path "${BRIDGE_MANIFEST}" --quiet
BIN="${ROOT}/bridge/target/debug/thatsmatter-bridge"

echo "==> smoke_rs_matter: start --matter-backend rs_matter on ${LISTEN}"
start_bridge "${OUT_DIR}/bridge-rs-matter.log"

curl -sf "${BASE}/health" | tee "${OUT_DIR}/health.json"
echo
curl -sf "${BASE}/status" | tee "${OUT_DIR}/status.json"
echo
curl -sf "${BASE}/pairing" | tee "${OUT_DIR}/pairing.json"
echo

echo "==> smoke_rs_matter: restart on the same data dir"
stop_bridge
start_bridge "${OUT_DIR}/bridge-rs-matter-restart.log"
curl -sf "${BASE}/pairing" | tee "${OUT_DIR}/pairing-restart.json"
echo

python3 -c '
import json, sys
status = json.load(open(sys.argv[1]))
pairing = json.load(open(sys.argv[2]))
restarted = json.load(open(sys.argv[3]))

# Matter Core spec: setup passcode range and the values the spec forbids.
INVALID_PASSCODES = {
    0, 11111111, 22222222, 33333333, 44444444, 55555555,
    66666666, 77777777, 88888888, 99999999, 12345678, 87654321,
}

assert status["running"] is True
assert status["matter_backend"] == "rs_matter"
assert status["export_count"] == 0
assert pairing["setup_code"]
assert pairing["qr_payload"].startswith("MT:")
assert 1 <= pairing["passcode"] <= 99999998, pairing["passcode"]
assert pairing["passcode"] not in INVALID_PASSCODES, pairing["passcode"]
assert 0 <= pairing["discriminator"] <= 4095, pairing["discriminator"]
assert restarted == pairing, (pairing, restarted)
print("smoke_rs_matter: PASS")
print("  setup_code=", pairing["setup_code"])
print("  discriminator=", pairing["discriminator"])
print("  qr_payload=", pairing["qr_payload"][:40] + "...")
print("  stable across restart on the same data dir")
' "${OUT_DIR}/status.json" "${OUT_DIR}/pairing.json" "${OUT_DIR}/pairing-restart.json"
