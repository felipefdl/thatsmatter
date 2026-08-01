#!/usr/bin/env bash
# Install ThatsMatter App metadata + integration onto HAOS.
# Prefer git add-on repo + HACS when online (pulls GHCR image).
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
if [[ -d "${ROOT}/thatsmatter" && -f "${ROOT}/thatsmatter/config.yaml" ]]; then
  PKG="${ROOT}"
elif [[ -d "${ROOT}/dist/ha-addons/thatsmatter" ]]; then
  PKG="${ROOT}/dist/ha-addons"
else
  echo "==> packaging first..."
  bash "${ROOT}/scripts/package-haos.sh" "${ROOT}/dist/ha-addons"
  PKG="${ROOT}/dist/ha-addons"
fi

HA_ADDONS_DIR="${HA_ADDONS_DIR:-/addons}"
HA_CONFIG_DIR="${HA_CONFIG_DIR:-/config}"

install_local() {
  echo "==> Installing App metadata to ${HA_ADDONS_DIR}/thatsmatter"
  mkdir -p "${HA_ADDONS_DIR}"
  rm -rf "${HA_ADDONS_DIR}/thatsmatter"
  cp -a "${PKG}/thatsmatter" "${HA_ADDONS_DIR}/thatsmatter"
  chmod a+x "${HA_ADDONS_DIR}/thatsmatter/run.sh" 2>/dev/null || true

  echo "==> Installing integration to ${HA_CONFIG_DIR}/custom_components/thatsmatter"
  mkdir -p "${HA_CONFIG_DIR}/custom_components"
  rm -rf "${HA_CONFIG_DIR}/custom_components/thatsmatter"
  cp -a "${PKG}/custom_components/thatsmatter" "${HA_CONFIG_DIR}/custom_components/thatsmatter"

  echo "==> Install complete"
  echo
  echo "Next:"
  echo "  1. Apps → refresh → Install/Start ThatsMatter (pulls ghcr.io/felipefdl/thatsmatter)"
  echo "  2. Restart Home Assistant"
  echo "  3. Devices & services → Add ThatsMatter"
  if command -v ha >/dev/null 2>&1; then
    ha addons reload || true
  fi
}

if [[ -n "${HA_SSH:-}" ]]; then
  echo "==> Remote install via ${HA_SSH}"
  REMOTE_TMP="${REMOTE_TMP:-/tmp/thatsmatter-haos-$$}"
  ssh "${HA_SSH}" "rm -rf '${REMOTE_TMP}' && mkdir -p '${REMOTE_TMP}'"
  if command -v rsync >/dev/null 2>&1; then
    rsync -az --delete "${PKG}/" "${HA_SSH}:${REMOTE_TMP}/"
  else
    tar -C "${PKG}" -czf - . | ssh "${HA_SSH}" "tar -C '${REMOTE_TMP}' -xzf -"
  fi
  ssh "${HA_SSH}" "bash -s" <<EOF
set -euo pipefail
HA_ADDONS_DIR='${HA_ADDONS_DIR}'
HA_CONFIG_DIR='${HA_CONFIG_DIR}'
PKG='${REMOTE_TMP}'
mkdir -p "\${HA_ADDONS_DIR}" "\${HA_CONFIG_DIR}/custom_components"
rm -rf "\${HA_ADDONS_DIR}/thatsmatter"
cp -a "\${PKG}/thatsmatter" "\${HA_ADDONS_DIR}/thatsmatter"
rm -rf "\${HA_CONFIG_DIR}/custom_components/thatsmatter"
cp -a "\${PKG}/custom_components/thatsmatter" "\${HA_CONFIG_DIR}/custom_components/thatsmatter"
if command -v ha >/dev/null 2>&1; then ha addons reload || true; fi
echo "Remote install OK"
EOF
else
  install_local
fi
