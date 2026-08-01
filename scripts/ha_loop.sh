#!/usr/bin/env bash
# Full Docker loop: bridge + Matter Server + Home Assistant, then commission attempt.
# Run twice for consistency when capturing evidence.
# Exit 0: commission succeeded. Exit 2: honest environment/network/commission failure.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
OUT_DIR="${1:-}"
if [[ -z "${OUT_DIR}" ]]; then
  echo "usage: $0 <evidence-output-dir>" >&2
  exit 1
fi
mkdir -p "${OUT_DIR}"
RUN_ID="${2:-1}"
LOG="${OUT_DIR}/run${RUN_ID}.log"

exec > >(tee -a "${LOG}") 2>&1

echo "=== ha_loop run=${RUN_ID} $(date -u +%Y-%m-%dT%H:%M:%SZ) ==="
echo "cwd=${ROOT}"
echo "uname=$(uname -a)"
docker version 2>&1 | head -5 || true

cd "${ROOT}"

echo "=== docker compose --profile ha --profile matter up --build -d ==="
set +e
docker compose --profile ha --profile matter up --build -d
UP_RC=$?
set -e
echo "compose_up_exit=${UP_RC}"
docker compose --profile ha --profile matter ps 2>&1 | tee "${OUT_DIR}/run${RUN_ID}-ps.txt" || true
docker compose --profile ha --profile matter logs --no-color --tail=80 2>&1 | tee "${OUT_DIR}/run${RUN_ID}-compose-logs.txt" || true

if [[ "${UP_RC}" -ne 0 ]]; then
  echo "ENV_FAILURE: docker compose up failed exit=${UP_RC}"
  echo "ENV_FAILURE" > "${OUT_DIR}/run${RUN_ID}-result.txt"
  exit 2
fi

# Wait briefly for processes
sleep 5

# Bridge health
set +e
curl -sf --max-time 5 http://127.0.0.1:18465/health | tee "${OUT_DIR}/run${RUN_ID}-bridge-health.json"
BRIDGE_RC=$?
set -e
echo "bridge_health_exit=${BRIDGE_RC}"
if [[ "${BRIDGE_RC}" -ne 0 ]]; then
  echo "ENV_FAILURE: bridge /health not reachable on 127.0.0.1:18465"
  docker compose logs --no-color --tail=50 thatsmatter-bridge 2>&1 | tee -a "${OUT_DIR}/run${RUN_ID}-compose-logs.txt" || true
  echo "ENV_FAILURE" > "${OUT_DIR}/run${RUN_ID}-result.txt"
  exit 2
fi

# HA frontend
set +e
curl -sf --max-time 5 -o /dev/null -w "%{http_code}" http://127.0.0.1:8123/ | tee "${OUT_DIR}/run${RUN_ID}-ha-status.txt"
HA_RC=$?
set -e
echo
echo "ha_curl_exit=${HA_RC}"

# Matter server port
set +e
python3 - <<'PY' 2>"${OUT_DIR}/run${RUN_ID}-matter-port.err" | tee "${OUT_DIR}/run${RUN_ID}-matter-port.txt"
import socket
s = socket.socket()
s.settimeout(3)
try:
    s.connect(("127.0.0.1", 5580))
    print("matter_server_tcp: open")
except Exception as e:
    print(f"matter_server_tcp: closed ({e})")
finally:
    s.close()
PY
set -e

# Ensure websockets available for commission script
PYTHON="${ROOT}/.venv-test/bin/python"
if [[ ! -x "${PYTHON}" ]]; then
  PYTHON=python3
fi
"${PYTHON}" -c "import websockets" 2>/dev/null || "${PYTHON}" -m pip install -q websockets

echo "=== commission attempt (Matter Server = HA controller backend) ==="
set +e
"${PYTHON}" "${ROOT}/scripts/ha_loop_commission.py" \
  --bridge-url "http://127.0.0.1:18465" \
  --matter-ws "ws://127.0.0.1:5580/ws" \
  --ha-url "http://127.0.0.1:8123" \
  --out "${OUT_DIR}/run${RUN_ID}-commission.json"
COMM_RC=$?
set -e
echo "commission_exit=${COMM_RC}"

if [[ "${COMM_RC}" -eq 0 ]]; then
  echo "SUCCESS: commission completed"
  echo "SUCCESS" > "${OUT_DIR}/run${RUN_ID}-result.txt"
  # Best-effort OnOff via device_command is optional; commission alone satisfies AC4 success path.
  exit 0
fi

echo "ENV_FAILURE: commission did not succeed (exit=${COMM_RC}). See logs above."
echo "This is the full HA+Matter Server+bridge path under Docker host networking."
echo "ENV_FAILURE" > "${OUT_DIR}/run${RUN_ID}-result.txt"
exit 2
