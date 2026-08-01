"""Constants for the ThatsMatter integration."""

from __future__ import annotations

DOMAIN = "thatsmatter"

# Default loopback IPC endpoint for the external bridge process.
DEFAULT_BRIDGE_HOST = "127.0.0.1"
DEFAULT_BRIDGE_PORT = 18465
DEFAULT_BRIDGE_NAME = "ThatsMatter"

CONF_BRIDGE_HOST = "bridge_host"
CONF_BRIDGE_PORT = "bridge_port"
CONF_BRIDGE_NAME = "bridge_name"

STORAGE_KEY = "thatsmatter_exports"
STORAGE_VERSION = 1

# How often the component polls the bridge for Matter -> HA commands.
COMMAND_POLL_INTERVAL = 0.5

# How often the component refreshes bridge status and pairing material.
STATUS_POLL_INTERVAL = 5.0

# Service names (domain is DOMAIN).
SERVICE_ADD_EXPORT = "add_export"
SERVICE_UPDATE_EXPORT = "update_export"
SERVICE_REMOVE_EXPORT = "remove_export"
SERVICE_SET_ENABLED = "set_enabled"
SERVICE_RESET_NAME_FROM_HA = "reset_name_from_ha"

ATTR_ENTITY_ID = "entity_id"
ATTR_EXPORT_ID = "export_id"
ATTR_NAME = "name"
ATTR_TYPE = "type"
ATTR_PRIMARY_ENTITY_ID = "primary_entity_id"
ATTR_LINKED = "linked"
ATTR_AREA_ID = "area_id"
ATTR_ENABLED = "enabled"
