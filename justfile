# ThatsMatter monorepo recipes

bridge_manifest := "bridge/Cargo.toml"
default_listen := "127.0.0.1:18465"
default_data_dir := "./data"
python := env("PYTHON", ".venv-test/bin/python")

# Build the Rust bridge binary
bridge-build:
    cargo build --manifest-path {{bridge_manifest}}

# Run bridge unit and integration tests
bridge-test:
    cargo test --manifest-path {{bridge_manifest}}

# Run the bridge on loopback (override with LISTEN= and DATA_DIR=)
bridge-run LISTEN=default_listen DATA_DIR=default_data_dir:
    cargo run --manifest-path {{bridge_manifest}} -- --listen {{LISTEN}} --data-dir {{DATA_DIR}}

# Live IPC smoke: start bridge (dev), create export, push state, status/pairing, delete
smoke:
    bash scripts/smoke_ipc.sh

# Commissionable backend smoke: rs_matter stack + real pairing material
smoke-matter:
    bash scripts/smoke_rs_matter.sh

# Python unit tests for the custom component (no Home Assistant runtime)
ha-test:
    {{python}} -m pytest custom_components/thatsmatter/tests -q

# Home Assistant / Python lint
ha-lint:
    {{python}} -m ruff check .

# Cargo + Python unit tests
test: bridge-test ha-test

# Full local verify without a live HA instance
verify: test smoke smoke-matter ha-lint

# Docker HA + Matter Server + bridge (Linux host network recommended)
docker-up:
    docker compose --profile ha --profile matter up --build

# Full loop: start stack and attempt Matter commission (pass OUT= dir)
ha-loop OUT="ha-loop-out":
    bash scripts/ha_loop.sh {{OUT}} 1
    bash scripts/ha_loop.sh {{OUT}} 2

# Assemble HAOS App metadata + integration under dist/ha-addons
package-haos:
    bash scripts/package-haos.sh

# Install onto local HAOS paths (/addons + /config) or HA_SSH=host
install-haos:
    bash scripts/install-haos.sh

# Build multi-arch note: use GH Actions for GHCR; local single-arch:
ghcr-local TAG="dev":
    docker build -f addons/thatsmatter/Dockerfile -t ghcr.io/felipefdl/thatsmatter:{{TAG}} .
