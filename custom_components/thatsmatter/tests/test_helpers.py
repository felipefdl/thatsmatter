"""Tests for device type defaults and pure helpers (no Home Assistant)."""

from __future__ import annotations

import sys
from pathlib import Path

import pytest

# Allow importing the integration package without installing into site-packages.
_ROOT = Path(__file__).resolve().parents[3]
if str(_ROOT) not in sys.path:
    sys.path.insert(0, str(_ROOT))

from custom_components.thatsmatter.helpers import (  # noqa: E402
    create_export_body,
    default_type_for_entity,
    domain_from_entity_id,
    export_to_protocol_dict,
    ha_brightness_to_matter_level,
    is_supported_type,
    matter_level_to_ha_brightness,
    pairing_credentials_for_display,
    pairing_notification_action,
    should_show_pairing_notification,
    validate_export_fields,
)


@pytest.mark.parametrize(
    ("entity_id", "device_class", "expected"),
    [
        ("light.kitchen", None, "light"),
        ("light.kitchen", "on_off", "light"),
        ("switch.plug", "outlet", "outlet"),
        ("switch.wall", None, "on_off_switch"),
        ("switch.wall", "switch", "on_off_switch"),
        ("input_boolean.guest_mode", None, "on_off_switch"),
        ("cover.shade", None, "cover"),
        ("cover.shade", "blind", "cover"),
        ("cover.garage_door", "garage", "garage"),
        ("cover.driveway", "gate", "garage"),
        ("binary_sensor.front_door", "door", "contact"),
        ("binary_sensor.window", "window", "contact"),
        ("binary_sensor.opening", "opening", "contact"),
        ("binary_sensor.motion_hall", "motion", "motion"),
        ("binary_sensor.occ", "occupancy", "motion"),
        ("binary_sensor.unknown", None, None),
        ("binary_sensor.battery", "battery", None),
        ("sensor.temp", "temperature", None),
        ("climate.living", None, None),
        ("camera.front", None, None),
        ("not_an_entity", None, None),
    ],
)
def test_default_type_for_entity(
    entity_id: str, device_class: str | None, expected: str | None
) -> None:
    assert default_type_for_entity(entity_id, device_class) == expected


def test_default_type_accepts_explicit_domain() -> None:
    assert (
        default_type_for_entity(
            "weird_id", device_class="outlet", domain="switch"
        )
        == "outlet"
    )


def test_domain_from_entity_id() -> None:
    assert domain_from_entity_id("light.kitchen") == "light"
    assert domain_from_entity_id("no_dot") == ""


def test_is_supported_type() -> None:
    assert is_supported_type("light")
    assert is_supported_type("on_off_switch")
    assert not is_supported_type("camera")
    assert not is_supported_type("thermostat")


def test_validate_export_fields_ok() -> None:
    errors = validate_export_fields(
        name="Kitchen Lamp",
        type_key="light",
        primary_entity_id="light.kitchen",
    )
    assert errors == []


def test_validate_export_fields_rejects_empty_name_and_bad_type() -> None:
    errors = validate_export_fields(
        name="  ",
        type_key="camera",
        primary_entity_id="bad",
    )
    assert any("name" in e for e in errors)
    assert any("unsupported type" in e for e in errors)
    assert any("primary_entity_id" in e for e in errors)


def test_export_to_protocol_dict_and_create_body() -> None:
    export = {
        "export_id": "11111111-1111-4111-8111-111111111111",
        "name": "Kitchen Lamp",
        "type": "light",
        "primary_entity_id": "light.kitchen",
        "linked": {"battery": "sensor.bat"},
        "area_id": "kitchen",
        "enabled": True,
        "endpoint_id": 3,
    }
    proto = export_to_protocol_dict(export)
    assert proto["export_id"] == export["export_id"]
    assert proto["type"] == "light"
    assert proto["linked"]["battery"] == "sensor.bat"
    assert proto["endpoint_id"] == 3

    body = create_export_body(export)
    assert body["export_id"] == export["export_id"]
    assert "endpoint_id" not in body
    assert body["area_id"] == "kitchen"


def test_brightness_round_trip_edges() -> None:
    assert ha_brightness_to_matter_level(0) == 0
    assert ha_brightness_to_matter_level(255) == 254
    assert matter_level_to_ha_brightness(0) == 0
    assert matter_level_to_ha_brightness(254) == 255
    mid = ha_brightness_to_matter_level(128)
    assert 0 < mid < 254


def test_pairing_credentials_withheld_when_open_fails() -> None:
    """Options flow must not display setup code/QR unless open succeeded and status is open."""
    material = {"setup_code": "1234-567-8901", "qr_payload": "MT:ABC"}
    assert (
        pairing_credentials_for_display(
            open_ok=False,
            pairing_open=False,
            pairing=material,
        )
        is None
    )
    # Open call returned success but status still reports closed.
    assert (
        pairing_credentials_for_display(
            open_ok=True,
            pairing_open=False,
            pairing=material,
        )
        is None
    )
    # Open failed even if a stale pairing_open flag were true.
    assert (
        pairing_credentials_for_display(
            open_ok=False,
            pairing_open=True,
            pairing=material,
        )
        is None
    )


def test_pairing_credentials_shown_only_when_open_and_confirmed() -> None:
    material = {"setup_code": "1234-567-8901", "qr_payload": "MT:ABC"}
    assert pairing_credentials_for_display(
        open_ok=True,
        pairing_open=True,
        pairing=material,
    ) == ("1234-567-8901", "MT:ABC")
    # Missing material still withholds.
    assert (
        pairing_credentials_for_display(
            open_ok=True,
            pairing_open=True,
            pairing={},
        )
        is None
    )


def test_should_show_pairing_notification_gating() -> None:
    """Notification stays only while connected + window open + code present; else dismiss."""
    assert should_show_pairing_notification(
        bridge_connected=True,
        pairing_open=True,
        has_setup_code=True,
    )
    assert not should_show_pairing_notification(
        bridge_connected=False,
        pairing_open=True,
        has_setup_code=True,
    )
    assert not should_show_pairing_notification(
        bridge_connected=True,
        pairing_open=False,
        has_setup_code=True,
    )
    assert not should_show_pairing_notification(
        bridge_connected=True,
        pairing_open=True,
        has_setup_code=False,
    )


def test_pairing_notification_action_dismisses_when_closed_or_disconnected() -> None:
    """Reconciliation returns dismiss (not merely a false show-predicate).

    Coordinator maps this action to async_dismiss_pairing_notification.
    """
    assert (
        pairing_notification_action(
            bridge_connected=True,
            pairing_open=False,
            setup_code="1234-567-8901",
            last_notified_code="1234-567-8901",
        )
        == "dismiss"
    )
    assert (
        pairing_notification_action(
            bridge_connected=False,
            pairing_open=True,
            setup_code="1234-567-8901",
            last_notified_code="1234-567-8901",
        )
        == "dismiss"
    )
    assert (
        pairing_notification_action(
            bridge_connected=True,
            pairing_open=True,
            setup_code=None,
            last_notified_code="1234-567-8901",
        )
        == "dismiss"
    )


def test_pairing_notification_action_create_and_noop() -> None:
    """Show when open+connected+code; noop when the same code is already shown."""
    assert (
        pairing_notification_action(
            bridge_connected=True,
            pairing_open=True,
            setup_code="1234-567-8901",
            last_notified_code=None,
        )
        == "create"
    )
    assert (
        pairing_notification_action(
            bridge_connected=True,
            pairing_open=True,
            setup_code="1234-567-8901",
            last_notified_code="1234-567-8901",
        )
        == "noop"
    )
    assert (
        pairing_notification_action(
            bridge_connected=True,
            pairing_open=True,
            setup_code="9999-888-7777",
            last_notified_code="1234-567-8901",
        )
        == "create"
    )
