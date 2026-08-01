#!/usr/bin/env bash
# Assemble HAOS install bundle (add-on metadata + integration). App image comes from GHCR.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
OUT="${1:-${ROOT}/dist/ha-addons}"
APP_SRC="${ROOT}/addons/thatsmatter"

echo "==> package-haos: ${OUT}"
rm -rf "${OUT}"
mkdir -p "${OUT}/thatsmatter"

cp -a "${APP_SRC}/config.yaml" "${OUT}/thatsmatter/"
cp -a "${APP_SRC}/DOCS.md" "${OUT}/thatsmatter/"
cp -a "${APP_SRC}/CHANGELOG.md" "${OUT}/thatsmatter/"
cp -a "${APP_SRC}/README.md" "${OUT}/thatsmatter/"
cp -a "${APP_SRC}/run.sh" "${OUT}/thatsmatter/"
cp -a "${APP_SRC}/Dockerfile" "${OUT}/thatsmatter/" 2>/dev/null || true
cp -a "${APP_SRC}/translations" "${OUT}/thatsmatter/"
chmod a+x "${OUT}/thatsmatter/run.sh"

# Repository index (also at monorepo root for git add-on store)
cp -a "${ROOT}/repository.yaml" "${OUT}/repository.yaml"

mkdir -p "${OUT}/custom_components"
cp -a "${ROOT}/custom_components/thatsmatter" "${OUT}/custom_components/"
find "${OUT}/custom_components" -type d -name '__pycache__' -exec rm -rf {} + 2>/dev/null || true
find "${OUT}/custom_components" -type f -name '*.pyc' -delete 2>/dev/null || true

cp -a "${ROOT}/scripts/install-haos.sh" "${OUT}/install-haos.sh"
chmod a+x "${OUT}/install-haos.sh"

cat > "${OUT}/README.md" <<'EOF'
# ThatsMatter HAOS package

- `thatsmatter/` — App metadata (image: `ghcr.io/felipefdl/thatsmatter`)
- `custom_components/thatsmatter/` — integration for HACS/manual install
- `repository.yaml` — add-on store repository root

## Install App (recommended)

Add this GitHub repo as an add-on repository in HA:

1. Settings → System → Apps → Add-on store → ⋮ → Repositories
2. Add: `https://github.com/felipefdl/thatsmatter`
3. Install **ThatsMatter** → Start (pulls GHCR image; no local Rust compile)

## Install integration (HACS)

1. HACS → Integrations → ⋮ → Custom repositories
2. Repository: `felipefdl/thatsmatter` (or full URL), category **Integration**
3. Download **ThatsMatter** → Restart HA
4. Settings → Devices & services → Add **ThatsMatter**

## Local copy (SSH)

```bash
sudo bash install-haos.sh
```

Copies App metadata to `/addons/thatsmatter` and integration to `/config/custom_components/thatsmatter`.
Prefer the git repository + HACS flow when online.
EOF

echo "==> package-haos: done"
echo "    App metadata: ${OUT}/thatsmatter (image ghcr.io/felipefdl/thatsmatter)"
echo "    Integration:  ${OUT}/custom_components/thatsmatter"
