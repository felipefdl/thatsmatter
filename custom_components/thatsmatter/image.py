"""Pairing QR code image entity (shown on the ThatsMatter device)."""

from __future__ import annotations

import io
import logging

from homeassistant.components.image import ImageEntity
from homeassistant.config_entries import ConfigEntry
from homeassistant.core import HomeAssistant, callback
from homeassistant.helpers.entity import DeviceInfo
from homeassistant.helpers.entity_platform import AddEntitiesCallback
from homeassistant.util import dt as dt_util

from .const import DOMAIN
from .coordinator import ThatsMatterRuntime

_LOGGER = logging.getLogger(__name__)


async def async_setup_entry(
    hass: HomeAssistant,
    entry: ConfigEntry,
    async_add_entities: AddEntitiesCallback,
) -> None:
    """Set up pairing QR image."""
    runtime: ThatsMatterRuntime = hass.data[DOMAIN][entry.entry_id]
    async_add_entities([ThatsMatterPairingQrImage(hass, runtime, entry)])


class ThatsMatterPairingQrImage(ImageEntity):
    """PNG QR for the Matter onboarding payload."""

    _attr_has_entity_name = True
    _attr_name = "Pairing QR code"
    _attr_translation_key = "pairing_qr"
    _attr_content_type = "image/png"

    def __init__(
        self,
        hass: HomeAssistant,
        runtime: ThatsMatterRuntime,
        entry: ConfigEntry,
    ) -> None:
        super().__init__(hass)
        self._runtime = runtime
        self._entry = entry
        self._attr_unique_id = f"{entry.entry_id}_pairing_qr"
        self._attr_device_info = DeviceInfo(
            identifiers={(DOMAIN, entry.entry_id)},
            name=runtime.bridge_name,
            manufacturer="ThatsMatter",
            model="Matter bridge",
        )
        self._png: bytes | None = None
        self._payload: str | None = None
        self._attr_image_last_updated = dt_util.utcnow()

    async def async_added_to_hass(self) -> None:
        await super().async_added_to_hass()
        self._runtime.add_listener(self._handle_runtime_update)
        self._refresh_png()

    async def async_will_remove_from_hass(self) -> None:
        self._runtime.remove_listener(self._handle_runtime_update)
        await super().async_will_remove_from_hass()

    @callback
    def _handle_runtime_update(self) -> None:
        if self._refresh_png():
            self.async_write_ha_state()

    def _refresh_png(self) -> bool:
        """Regenerate PNG when QR payload changes. Returns True if changed."""
        payload = self._runtime.pairing.get("qr_payload")
        payload_s = str(payload) if payload else None
        if payload_s == self._payload and self._png is not None:
            return False
        self._payload = payload_s
        if not payload_s:
            self._png = None
            self._attr_image_last_updated = dt_util.utcnow()
            return True
        try:
            import segno

            qr = segno.make(payload_s, error="m")
            buf = io.BytesIO()
            qr.save(buf, kind="png", scale=8, border=2)
            self._png = buf.getvalue()
            self._attr_image_last_updated = dt_util.utcnow()
            return True
        except Exception:  # noqa: BLE001
            _LOGGER.exception("Failed to render pairing QR")
            self._png = None
            return True

    async def async_image(self) -> bytes | None:
        """Return PNG bytes for the current pairing payload."""
        if self._png is None:
            self._refresh_png()
        return self._png

    @property
    def available(self) -> bool:
        return self._runtime.pairing_window_open and bool(
            self._runtime.pairing.get("qr_payload")
        )
