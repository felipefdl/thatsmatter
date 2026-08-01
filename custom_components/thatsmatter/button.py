"""Button entities for bridge control actions."""

from __future__ import annotations

import logging

from homeassistant.components.button import ButtonEntity
from homeassistant.config_entries import ConfigEntry
from homeassistant.core import HomeAssistant, callback
from homeassistant.helpers.entity import DeviceInfo, EntityCategory
from homeassistant.helpers.entity_platform import AddEntitiesCallback

from .const import DOMAIN
from .coordinator import ThatsMatterRuntime

_LOGGER = logging.getLogger(__name__)


async def async_setup_entry(
    hass: HomeAssistant,
    entry: ConfigEntry,
    async_add_entities: AddEntitiesCallback,
) -> None:
    """Set up ThatsMatter buttons."""
    runtime: ThatsMatterRuntime = hass.data[DOMAIN][entry.entry_id]
    async_add_entities([ThatsMatterOpenPairingButton(runtime, entry)])


class ThatsMatterOpenPairingButton(ButtonEntity):
    """Open the Matter basic commissioning window so other apps can pair."""

    _attr_has_entity_name = True
    _attr_name = "Open pairing window"
    _attr_translation_key = "open_pairing"
    _attr_entity_category = EntityCategory.CONFIG
    _attr_should_poll = False

    def __init__(self, runtime: ThatsMatterRuntime, entry: ConfigEntry) -> None:
        self._runtime = runtime
        self._entry = entry
        self._attr_unique_id = f"{entry.entry_id}_open_pairing"
        self._attr_device_info = DeviceInfo(
            identifiers={(DOMAIN, entry.entry_id)},
            name=runtime.bridge_name,
            manufacturer="ThatsMatter",
            model="Matter bridge",
        )

    async def async_added_to_hass(self) -> None:
        await super().async_added_to_hass()
        self._runtime.add_listener(self._handle_runtime_update)

    async def async_will_remove_from_hass(self) -> None:
        self._runtime.remove_listener(self._handle_runtime_update)
        await super().async_will_remove_from_hass()

    @callback
    def _handle_runtime_update(self) -> None:
        self.async_write_ha_state()

    @property
    def available(self) -> bool:
        return self._runtime.bridge_connected

    async def async_press(self) -> None:
        """Open the pairing window and refresh pairing surfaces."""
        try:
            await self._runtime.async_open_pairing_window()
        except Exception:  # noqa: BLE001
            _LOGGER.exception("Failed to open pairing window")
            raise
