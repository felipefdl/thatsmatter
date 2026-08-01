"""Diagnostic sensors: bridge status and Matter pairing code."""

from __future__ import annotations

from homeassistant.components.sensor import SensorEntity
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
    """Set up ThatsMatter sensors."""
    runtime: ThatsMatterRuntime = hass.data[DOMAIN][entry.entry_id]
    async_add_entities(
        [
            ThatsMatterStatusSensor(runtime, entry),
            ThatsMatterPairingCodeSensor(runtime, entry),
            ThatsMatterExportCountSensor(runtime, entry),
        ]
    )


class ThatsMatterBaseSensor(SensorEntity):
    """Shared device info and runtime push wiring for bridge diagnostics."""

    _attr_has_entity_name = True
    _attr_entity_category = EntityCategory.DIAGNOSTIC
    # Runtime pushes updates; polling only added latency to bridge transitions.
    _attr_should_poll = False

    def __init__(self, runtime: ThatsMatterRuntime, entry: ConfigEntry) -> None:
        self._runtime = runtime
        self._entry = entry
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
        return True


class ThatsMatterStatusSensor(ThatsMatterBaseSensor):
    """Human-readable bridge connection / runtime status."""

    _attr_name = "Bridge status"
    _attr_translation_key = "bridge_status"

    def __init__(self, runtime: ThatsMatterRuntime, entry: ConfigEntry) -> None:
        super().__init__(runtime, entry)
        self._attr_unique_id = f"{entry.entry_id}_bridge_status"

    @property
    def native_value(self) -> str:
        if not self._runtime.bridge_connected:
            return "disconnected"
        status = self._runtime.bridge_status
        if status.get("error"):
            return "error"
        if status.get("pairing_open"):
            return "pairing_open"
        if status.get("running"):
            return "running"
        return "connected"

    @property
    def extra_state_attributes(self) -> dict:
        status = self._runtime.bridge_status
        return {
            "bridge_host": self._runtime.host,
            "bridge_port": self._runtime.port,
            "bridge_name": self._runtime.bridge_name,
            "matter_backend": status.get("matter_backend"),
            "export_count": status.get("export_count"),
            "enabled_export_count": status.get("enabled_export_count"),
            "pairing_open": status.get("pairing_open"),
            "commissioned_fabrics": status.get("commissioned_fabrics"),
            "error": status.get("error") or self._runtime.last_error,
            "local_export_count": len(self._runtime.store.list_exports()),
        }


class ThatsMatterPairingCodeSensor(ThatsMatterBaseSensor):
    """Manual Matter setup code (primary pairing surface on the device page)."""

    _attr_name = "Setup code"
    _attr_translation_key = "pairing_code"
    # Not diagnostic: user should see this without hunting Advanced entities.
    _attr_entity_category = None

    def __init__(self, runtime: ThatsMatterRuntime, entry: ConfigEntry) -> None:
        super().__init__(runtime, entry)
        self._attr_unique_id = f"{entry.entry_id}_pairing_code"
        self._attr_entity_category = None

    @property
    def native_value(self) -> str | None:
        # Only surface the code while the commissioning window is open.
        if not self._runtime.pairing_window_open:
            return None
        code = self._runtime.pairing.get("setup_code")
        return str(code) if code else None

    @property
    def extra_state_attributes(self) -> dict:
        pairing = self._runtime.pairing
        return {
            "qr_payload": pairing.get("qr_payload")
            if self._runtime.pairing_window_open
            else None,
            "discriminator": pairing.get("discriminator"),
            # passcode is sensitive; omit from attributes in production UIs if needed.
            "passcode": pairing.get("passcode")
            if self._runtime.pairing_window_open
            else None,
            "pairing_window_open": self._runtime.pairing_window_open,
        }


class ThatsMatterExportCountSensor(ThatsMatterBaseSensor):
    """Number of exports in the local HA catalog."""

    _attr_name = "Export count"
    _attr_translation_key = "export_count"
    _attr_native_unit_of_measurement = "exports"

    def __init__(self, runtime: ThatsMatterRuntime, entry: ConfigEntry) -> None:
        super().__init__(runtime, entry)
        self._attr_unique_id = f"{entry.entry_id}_export_count"

    @property
    def native_value(self) -> int:
        return len(self._runtime.store.list_exports())

    @property
    def extra_state_attributes(self) -> dict:
        exports = self._runtime.store.list_exports()
        return {
            "enabled_count": sum(1 for e in exports if e.enabled),
            "exports": [
                {
                    "export_id": e.export_id,
                    "name": e.name,
                    "type": e.type,
                    "primary_entity_id": e.primary_entity_id,
                    "enabled": e.enabled,
                    "endpoint_id": e.endpoint_id,
                }
                for e in exports
            ],
        }
