"""Binary sensors for bridge connectivity."""

from __future__ import annotations

from homeassistant.components.binary_sensor import (
    BinarySensorDeviceClass,
    BinarySensorEntity,
)
from homeassistant.config_entries import ConfigEntry
from homeassistant.core import HomeAssistant, callback
from homeassistant.helpers.entity import DeviceInfo, EntityCategory
from homeassistant.helpers.entity_platform import AddEntitiesCallback

from .const import DOMAIN
from .coordinator import ThatsMatterRuntime


async def async_setup_entry(
    hass: HomeAssistant,
    entry: ConfigEntry,
    async_add_entities: AddEntitiesCallback,
) -> None:
    """Set up ThatsMatter binary sensors."""
    runtime: ThatsMatterRuntime = hass.data[DOMAIN][entry.entry_id]
    async_add_entities([ThatsMatterConnectedSensor(runtime, entry)])


class ThatsMatterConnectedSensor(BinarySensorEntity):
    """Whether the external bridge IPC is currently reachable."""

    _attr_has_entity_name = True
    _attr_name = "Bridge connected"
    _attr_translation_key = "bridge_connected"
    _attr_device_class = BinarySensorDeviceClass.CONNECTIVITY
    _attr_entity_category = EntityCategory.DIAGNOSTIC
    # Runtime pushes updates; polling only added latency to bridge transitions.
    _attr_should_poll = False

    def __init__(self, runtime: ThatsMatterRuntime, entry: ConfigEntry) -> None:
        self._runtime = runtime
        self._entry = entry
        self._attr_unique_id = f"{entry.entry_id}_bridge_connected"
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
    def is_on(self) -> bool:
        return self._runtime.bridge_connected
