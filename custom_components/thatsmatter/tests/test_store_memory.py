"""In-memory ExportStore tests (no Home Assistant Store)."""

from __future__ import annotations

import asyncio
import sys
from pathlib import Path

_ROOT = Path(__file__).resolve().parents[3]
if str(_ROOT) not in sys.path:
    sys.path.insert(0, str(_ROOT))

from custom_components.thatsmatter.models import Export  # noqa: E402
from custom_components.thatsmatter.store import ExportStore  # noqa: E402


def test_memory_store_crud() -> None:
    async def _run() -> None:
        backend: dict = {}
        store = ExportStore(hass=None, bridge_name="ThatsMatter", backend=backend)
        await store.async_load()
        assert store.list_exports() == []

        exp = Export.new(
            name="Lamp",
            type_key="light",
            primary_entity_id="light.lamp",
            export_id="11111111-1111-4111-8111-111111111111",
        )
        await store.async_upsert(exp)
        assert store.get(exp.export_id) is not None
        assert backend["exports"][0]["name"] == "Lamp"

        # Reload from backend
        store2 = ExportStore(hass=None, bridge_name="ThatsMatter", backend=backend)
        await store2.async_load()
        assert store2.get(exp.export_id).name == "Lamp"  # type: ignore[union-attr]

        await store2.async_delete(exp.export_id)
        assert store2.get(exp.export_id) is None
        assert backend["exports"] == []

    asyncio.run(_run())


def test_apply_endpoint_ids() -> None:
    async def _run() -> None:
        store = ExportStore(hass=None, bridge_name="ThatsMatter", backend={})
        await store.async_load()
        exp = Export.new(
            name="Lamp",
            type_key="light",
            primary_entity_id="light.lamp",
            export_id="11111111-1111-4111-8111-111111111111",
        )
        await store.async_upsert(exp)
        changed = store.apply_endpoint_ids(
            [
                {
                    "export_id": exp.export_id,
                    "endpoint_id": 5,
                    "name": "Lamp",
                    "type": "light",
                    "primary_entity_id": "light.lamp",
                    "enabled": True,
                }
            ]
        )
        assert changed is True
        assert store.get(exp.export_id).endpoint_id == 5  # type: ignore[union-attr]

    asyncio.run(_run())
