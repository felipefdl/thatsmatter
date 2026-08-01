"""Tests for Export and CatalogData models (no Home Assistant)."""

from __future__ import annotations

import sys
from pathlib import Path

import pytest

_ROOT = Path(__file__).resolve().parents[3]
if str(_ROOT) not in sys.path:
    sys.path.insert(0, str(_ROOT))

from custom_components.thatsmatter.models import CatalogData, Export  # noqa: E402


def test_export_new_generates_stable_uuid_not_entity_id() -> None:
    exp = Export.new(
        name="Kitchen Lamp",
        type_key="light",
        primary_entity_id="light.kitchen",
    )
    assert exp.export_id != "light.kitchen"
    assert len(exp.export_id) == 36
    assert exp.enabled is True
    assert exp.linked == {}
    assert exp.endpoint_id is None


def test_export_new_rejects_unsupported_type() -> None:
    with pytest.raises(ValueError, match="unsupported"):
        Export.new(
            name="Cam",
            type_key="camera",
            primary_entity_id="camera.front",
        )


def test_export_new_rejects_empty_name() -> None:
    with pytest.raises(ValueError, match="name"):
        Export.new(
            name="",
            type_key="light",
            primary_entity_id="light.x",
        )


def test_export_protocol_round_trip() -> None:
    original = Export.new(
        name="Plug",
        type_key="outlet",
        primary_entity_id="switch.plug",
        linked={"battery": "sensor.plug_battery"},
        area_id="kitchen",
        enabled=False,
        export_id="22222222-2222-4222-8222-222222222222",
    )
    original.endpoint_id = 7
    data = original.to_protocol_dict()
    restored = Export.from_protocol_dict(data)
    assert restored.export_id == original.export_id
    assert restored.name == "Plug"
    assert restored.type == "outlet"
    assert restored.primary_entity_id == "switch.plug"
    assert restored.linked == {"battery": "sensor.plug_battery"}
    assert restored.area_id == "kitchen"
    assert restored.enabled is False
    assert restored.endpoint_id == 7


def test_catalog_upsert_delete_and_find() -> None:
    cat = CatalogData(bridge_name="ThatsMatter")
    a = Export.new(
        name="A",
        type_key="light",
        primary_entity_id="light.a",
        export_id="aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa",
    )
    b = Export.new(
        name="B",
        type_key="on_off_switch",
        primary_entity_id="switch.b",
        enabled=False,
        export_id="bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb",
    )
    cat.upsert(a)
    cat.upsert(b)
    assert len(cat.list_exports()) == 2
    assert cat.get(a.export_id) is a

    a2 = Export(
        export_id=a.export_id,
        name="A renamed",
        type="light",
        primary_entity_id="light.a",
        linked={},
        area_id=None,
        enabled=True,
        endpoint_id=1,
    )
    cat.upsert(a2)
    assert cat.get(a.export_id).name == "A renamed"  # type: ignore[union-attr]
    assert len(cat.list_exports()) == 2

    # Only enabled exports match entity lookup.
    assert cat.find_by_entity("light.a")[0].export_id == a.export_id
    assert cat.find_by_entity("switch.b") == []

    removed = cat.delete(b.export_id)
    assert removed is not None
    assert cat.get(b.export_id) is None


def test_catalog_storage_round_trip() -> None:
    cat = CatalogData(bridge_name="Home")
    cat.upsert(
        Export.new(
            name="Lamp",
            type_key="light",
            primary_entity_id="light.lamp",
            export_id="11111111-1111-4111-8111-111111111111",
        )
    )
    raw = cat.to_storage_dict()
    loaded = CatalogData.from_storage_dict(raw, default_bridge_name="ThatsMatter")
    assert loaded.bridge_name == "Home"
    assert len(loaded.exports) == 1
    assert loaded.exports[0].name == "Lamp"


def test_catalog_empty_storage() -> None:
    loaded = CatalogData.from_storage_dict(None, default_bridge_name="ThatsMatter")
    assert loaded.bridge_name == "ThatsMatter"
    assert loaded.exports == []


def test_enabled_primary_entity_ids_includes_linked() -> None:
    cat = CatalogData(bridge_name="ThatsMatter")
    cat.upsert(
        Export.new(
            name="Cover",
            type_key="cover",
            primary_entity_id="cover.shade",
            linked={"position": "sensor.shade_pos"},
            export_id="cccccccc-cccc-4ccc-8ccc-cccccccccccc",
        )
    )
    ids = cat.enabled_primary_entity_ids()
    assert "cover.shade" in ids
    assert "sensor.shade_pos" in ids
