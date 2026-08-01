"""Pure helpers: device type defaults and export validation (no Home Assistant import)."""

from __future__ import annotations

from typing import Any

# First-ship Matter device type keys (protocol DeviceType enum).
FIRST_SHIP_TYPES: frozenset[str] = frozenset(
    {
        "light",
        "on_off_switch",
        "on_off_plug",
        "outlet",
        "contact",
        "motion",
        "cover",
        "garage",
    }
)

# Domains that can back an export at add time.
SUPPORTED_DOMAINS: frozenset[str] = frozenset(
    {
        "light",
        "switch",
        "input_boolean",
        "cover",
        "binary_sensor",
    }
)

# binary_sensor device_class values -> contact
_CONTACT_CLASSES: frozenset[str] = frozenset(
    {"door", "window", "garage_door", "opening", "safety"}
)
# binary_sensor device_class values -> motion
_MOTION_CLASSES: frozenset[str] = frozenset({"motion", "occupancy", "presence", "moving"})
# cover device_class values -> garage
_GARAGE_CLASSES: frozenset[str] = frozenset({"garage", "gate"})


def domain_from_entity_id(entity_id: str) -> str:
    """Return the HA domain portion of an entity_id."""
    if "." not in entity_id:
        return ""
    return entity_id.split(".", 1)[0]


def default_type_for_entity(
    entity_id: str,
    device_class: str | None = None,
    domain: str | None = None,
) -> str | None:
    """Return the default Matter type key for an HA entity, or None if unsupported.

    Defaults follow docs/product-spec.md:
    - light.* -> light
    - switch.* with outlet device class -> outlet
    - switch.* / input_boolean.* -> on_off_switch
    - cover.* garage/gate -> garage
    - cover.* otherwise -> cover
    - binary_sensor contact/door/window -> contact
    - binary_sensor motion/occupancy -> motion
    """
    dom = domain if domain is not None else domain_from_entity_id(entity_id)
    if not dom:
        return None

    dc = (device_class or "").lower() or None

    if dom == "light":
        return "light"

    if dom == "switch":
        if dc == "outlet":
            return "outlet"
        return "on_off_switch"

    if dom == "input_boolean":
        return "on_off_switch"

    if dom == "cover":
        if dc in _GARAGE_CLASSES:
            return "garage"
        return "cover"

    if dom == "binary_sensor":
        if dc in _CONTACT_CLASSES or dc == "door" or dc == "window":
            return "contact"
        if dc in _MOTION_CLASSES:
            return "motion"
        # No device_class or unknown class: reject (ambiguous).
        return None

    return None


def is_supported_type(type_key: str) -> bool:
    """Return True if type_key is a first-ship Matter device type."""
    return type_key in FIRST_SHIP_TYPES


def validate_export_fields(
    *,
    name: str,
    type_key: str,
    primary_entity_id: str,
    linked: dict[str, str] | None = None,
) -> list[str]:
    """Return a list of validation error messages (empty if valid)."""
    errors: list[str] = []
    if not name or not name.strip():
        errors.append("name is required")
    elif len(name) > 64:
        errors.append("name must be at most 64 characters")
    if not is_supported_type(type_key):
        errors.append(f"unsupported type: {type_key}")
    if not primary_entity_id or "." not in primary_entity_id:
        errors.append("primary_entity_id must be a valid entity id")
    if linked:
        for role, eid in linked.items():
            if not isinstance(role, str) or not role:
                errors.append("linked roles must be non-empty strings")
            if not isinstance(eid, str) or "." not in eid:
                errors.append(f"linked entity for role {role!r} is invalid")
    return errors


def export_to_protocol_dict(export: dict[str, Any]) -> dict[str, Any]:
    """Normalize an export mapping to the protocol Export shape."""
    linked = export.get("linked") or {}
    return {
        "export_id": export["export_id"],
        "name": export["name"],
        "type": export["type"],
        "primary_entity_id": export["primary_entity_id"],
        "linked": dict(linked),
        "area_id": export.get("area_id"),
        "enabled": bool(export.get("enabled", True)),
        "endpoint_id": export.get("endpoint_id"),
    }


def create_export_body(export: dict[str, Any]) -> dict[str, Any]:
    """Body for POST /exports (CreateExport)."""
    body: dict[str, Any] = {
        "export_id": export["export_id"],
        "name": export["name"],
        "type": export["type"],
        "primary_entity_id": export["primary_entity_id"],
        "linked": dict(export.get("linked") or {}),
        "enabled": bool(export.get("enabled", True)),
    }
    area_id = export.get("area_id")
    if area_id is not None:
        body["area_id"] = area_id
    return body


def patch_export_body(
    *,
    name: str | None = None,
    type_key: str | None = None,
    primary_entity_id: str | None = None,
    linked: dict[str, str] | None = None,
    area_id: str | None | object = ...,
    enabled: bool | None = None,
) -> dict[str, Any]:
    """Body for PATCH /exports/{id} (only set fields)."""
    body: dict[str, Any] = {}
    if name is not None:
        body["name"] = name
    if type_key is not None:
        body["type"] = type_key
    if primary_entity_id is not None:
        body["primary_entity_id"] = primary_entity_id
    if linked is not None:
        body["linked"] = linked
    if area_id is not ...:
        body["area_id"] = area_id
    if enabled is not None:
        body["enabled"] = enabled
    return body


def ha_state_value(
    entity_id: str,
    state: str,
    attributes: dict[str, Any] | None = None,
) -> dict[str, Any]:
    """Build a protocol HaStateValue dict."""
    payload: dict[str, Any] = {
        "entity_id": entity_id,
        "state": state,
    }
    if attributes is not None:
        payload["attributes"] = attributes
    return payload


def matter_level_to_ha_brightness(level: int) -> int:
    """Map Matter level (0-254) to HA brightness (0-255)."""
    if level <= 0:
        return 0
    if level >= 254:
        return 255
    return max(1, round(level * 255 / 254))


def ha_brightness_to_matter_level(brightness: int) -> int:
    """Map HA brightness (0-255) to Matter level (0-254)."""
    if brightness <= 0:
        return 0
    if brightness >= 255:
        return 254
    return max(1, (brightness * 254) // 255)
