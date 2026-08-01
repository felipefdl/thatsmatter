"""ThatsMatter: Matter bridge for Home Assistant (export HA entities out)."""

from __future__ import annotations

import logging
from typing import TYPE_CHECKING, Any

from .const import (
    CONF_BRIDGE_HOST,
    CONF_BRIDGE_NAME,
    CONF_BRIDGE_PORT,
    DEFAULT_BRIDGE_HOST,
    DEFAULT_BRIDGE_NAME,
    DEFAULT_BRIDGE_PORT,
    DOMAIN,
)

if TYPE_CHECKING:
    from homeassistant.config_entries import ConfigEntry
    from homeassistant.core import HomeAssistant

_LOGGER = logging.getLogger(__name__)


def _platforms() -> list[Any]:
    """Entity platforms this integration sets up (import deferred for tests)."""
    from homeassistant.const import Platform

    return [Platform.SENSOR, Platform.BINARY_SENSOR, Platform.IMAGE]


async def async_setup(hass: HomeAssistant, config: dict[str, Any]) -> bool:
    """Set up the ThatsMatter domain (YAML not used; config entry only)."""
    hass.data.setdefault(DOMAIN, {})
    return True


async def async_setup_entry(hass: HomeAssistant, entry: ConfigEntry) -> bool:
    """Set up ThatsMatter from a config entry.

    Assumes an external bridge process is (or will be) listening on the
    configured host/port. Does not spawn the binary yet.
    """
    from .coordinator import ThatsMatterRuntime
    from .services import async_register_services
    from .store import ExportStore

    platforms = _platforms()

    hass.data.setdefault(DOMAIN, {})

    # Options override data for host/port/name after options flow edits.
    conf = {**entry.data, **entry.options}
    host = conf.get(CONF_BRIDGE_HOST, DEFAULT_BRIDGE_HOST)
    port = int(conf.get(CONF_BRIDGE_PORT, DEFAULT_BRIDGE_PORT))
    bridge_name = conf.get(CONF_BRIDGE_NAME, DEFAULT_BRIDGE_NAME)

    store = ExportStore(hass, bridge_name=bridge_name)
    runtime = ThatsMatterRuntime(
        hass,
        host=host,
        port=port,
        bridge_name=bridge_name,
        store=store,
    )
    hass.data[DOMAIN][entry.entry_id] = runtime

    async_register_services(hass)
    await runtime.async_start()

    await hass.config_entries.async_forward_entry_setups(entry, platforms)

    entry.async_on_unload(entry.add_update_listener(_async_update_listener))
    _LOGGER.info(
        "ThatsMatter started (bridge %s:%s, name=%s)", host, port, bridge_name
    )
    return True


async def async_unload_entry(hass: HomeAssistant, entry: ConfigEntry) -> bool:
    """Unload a ThatsMatter config entry."""
    from .services import async_unregister_services

    unload_ok = await hass.config_entries.async_unload_platforms(entry, _platforms())
    domain_data = hass.data[DOMAIN]
    runtime = domain_data.pop(entry.entry_id, None)
    if runtime is not None:
        await runtime.async_stop()
    # Services are domain-wide: drop them only once the last runtime is gone,
    # otherwise they linger in the UI and every call raises "not configured".
    if not any(not key.startswith("_") for key in domain_data):
        async_unregister_services(hass)
    return unload_ok


async def _async_update_listener(hass: HomeAssistant, entry: ConfigEntry) -> None:
    """Reload when options change."""
    await hass.config_entries.async_reload(entry.entry_id)
