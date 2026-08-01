"""HA .storage-backed export catalog."""

from __future__ import annotations

from typing import TYPE_CHECKING, Any

from .const import DEFAULT_BRIDGE_NAME, STORAGE_KEY, STORAGE_VERSION
from .models import CatalogData, Export

if TYPE_CHECKING:
    from homeassistant.core import HomeAssistant


class ExportStore:
    """Load/save the opt-in export catalog under HA .storage.

    When ``backend`` is provided (tests), persist only in that dict and skip
    the Home Assistant Store helper. That path needs no Home Assistant install.
    """

    def __init__(
        self,
        hass: HomeAssistant | None = None,
        *,
        bridge_name: str = DEFAULT_BRIDGE_NAME,
        backend: dict[str, Any] | None = None,
    ) -> None:
        self._hass = hass
        self._default_bridge_name = bridge_name
        self._backend = backend
        self._store: Any = None
        if hass is not None and backend is None:
            from homeassistant.helpers.storage import Store

            self._store = Store(hass, STORAGE_VERSION, STORAGE_KEY)
        self._data = CatalogData(bridge_name=bridge_name, exports=[])
        self._loaded = False

    @property
    def data(self) -> CatalogData:
        """Current in-memory catalog."""
        return self._data

    @property
    def bridge_name(self) -> str:
        """Bridge display name."""
        return self._data.bridge_name

    async def async_load(self) -> CatalogData:
        """Load from storage (or memory backend) into memory."""
        if self._backend is not None:
            self._data = CatalogData.from_storage_dict(
                self._backend, self._default_bridge_name
            )
            self._loaded = True
            return self._data

        raw: dict[str, Any] | None = None
        if self._store is not None:
            raw = await self._store.async_load()
        self._data = CatalogData.from_storage_dict(raw, self._default_bridge_name)
        self._loaded = True
        return self._data

    async def async_save(self) -> None:
        """Persist current catalog."""
        payload = self._data.to_storage_dict()
        if self._backend is not None:
            self._backend.clear()
            self._backend.update(payload)
            return
        if self._store is not None:
            await self._store.async_save(payload)

    def get(self, export_id: str) -> Export | None:
        """Return export by id."""
        return self._data.get(export_id)

    def list_exports(self) -> list[Export]:
        """All exports."""
        return self._data.list_exports()

    async def async_upsert(self, export: Export) -> Export:
        """Insert or replace and save."""
        self._data.upsert(export)
        await self.async_save()
        return export

    async def async_delete(self, export_id: str) -> Export | None:
        """Delete by id and save. Returns removed export or None."""
        removed = self._data.delete(export_id)
        if removed is not None:
            await self.async_save()
        return removed

    async def async_set_bridge_name(self, name: str) -> None:
        """Update bridge name and save."""
        cleaned = name.strip()
        if not cleaned or len(cleaned) > 32:
            raise ValueError("bridge_name must be 1..=32 characters")
        self._data.bridge_name = cleaned
        await self.async_save()

    def apply_endpoint_ids(self, bridge_exports: list[dict[str, Any]]) -> bool:
        """Copy endpoint_id from bridge responses into local store. Returns True if changed."""
        by_id = {str(item["export_id"]): item for item in bridge_exports}
        changed = False
        for exp in self._data.exports:
            remote = by_id.get(exp.export_id)
            if remote is None:
                continue
            ep = remote.get("endpoint_id")
            if ep is not None and exp.endpoint_id != ep:
                exp.endpoint_id = int(ep)
                changed = True
        return changed
