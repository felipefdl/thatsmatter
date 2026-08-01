#!/usr/bin/with-contenv bashio
# shellcheck shell=bash
set -e

BRIDGE_NAME="$(bashio::config 'bridge_name')"
LISTEN_PORT="$(bashio::config 'listen_port')"
MATTER_BACKEND="$(bashio::config 'matter_backend')"
LOG_LEVEL="$(bashio::config 'log_level')"
MDNS_INTERFACE="$(bashio::config 'mdns_interface')"

# Host network: bind all interfaces so Supervisor/Core can reach IPC.
LISTEN_ADDR="0.0.0.0:${LISTEN_PORT}"
DATA_DIR="/data"

bashio::log.info "ThatsMatter bridge starting"
bashio::log.info "  listen=${LISTEN_ADDR}"
bashio::log.info "  data_dir=${DATA_DIR}"
bashio::log.info "  backend=${MATTER_BACKEND}"
bashio::log.info "  name=${BRIDGE_NAME}"
if [ -n "${MDNS_INTERFACE}" ]; then
  bashio::log.info "  mdns_interface=${MDNS_INTERFACE}"
fi

# Advertise to Home Assistant for config flow discovery.
# On host_network, Core reaches the bridge via 127.0.0.1.
DISCOVERY_CONFIG=$(bashio::var.json \
  host "127.0.0.1" \
  port "^${LISTEN_PORT}" \
  bridge_name "${BRIDGE_NAME}")

if bashio::discovery "thatsmatter" "${DISCOVERY_CONFIG}"; then
  bashio::log.info "Published hassio discovery for thatsmatter"
else
  bashio::log.warning "Could not publish hassio discovery (integration can still be added manually)"
fi

export RUST_LOG="${LOG_LEVEL},thatsmatter_bridge=info,rs_matter=info"
export THATSMATTER_LISTEN="${LISTEN_ADDR}"
export THATSMATTER_DATA_DIR="${DATA_DIR}"
export THATSMATTER_BRIDGE_NAME="${BRIDGE_NAME}"
export THATSMATTER_MATTER_BACKEND="${MATTER_BACKEND}"
export THATSMATTER_ALLOW_NON_LOOPBACK="true"

if [ -n "${MDNS_INTERFACE}" ]; then
  export THATSMATTER_MDNS_INTERFACE="${MDNS_INTERFACE}"
fi

exec /usr/bin/thatsmatter-bridge \
  --listen "${LISTEN_ADDR}" \
  --allow-non-loopback \
  --data-dir "${DATA_DIR}" \
  --bridge-name "${BRIDGE_NAME}" \
  --matter-backend "${MATTER_BACKEND}"
