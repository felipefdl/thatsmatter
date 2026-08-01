"""Shared export catalog operations for UI flows and services (no YAML required)."""

from __future__ import annotations

import logging
from typing import Any

from homeassistant.core import HomeAssistant
from homeassistant.exceptions import HomeAssistantError
from homeassistant.helpers.entity_registry import async_get as async_get_entity_registry

from .helpers import (
    default_type_for_entity,
    domain_from_entity_id,
    is_supported_type,
    validate_export_fields,
)
from .models import Export
from .store import ExportStore

_LOGGER = logging.getLogger(__name__)

TYPE_LABELS: dict[str, str] = {
    "light": "Light",
    "on_off_switch": "Switch",
    "on_off_plug": "Plug",
    "outlet": "Outlet",
    "contact": "Contact sensor",
    "motion": "Motion sensor",
    "cover": "Blind / shade",
    "garage": "Garage door",
}


def get_runtime(hass: HomeAssistant) -> Any:
    """Return the single ThatsMatter runtime instance."""
    from .const import DOMAIN

    domain_data = hass.data.get(DOMAIN) or {}
    for key, runtime in domain_data.items():
        if key.startswith("_"):
            continue
        if hasattr(runtime, "store"):
            return runtime
    raise HomeAssistantError("ThatsMatter is not configured")


def friendly_name(hass: HomeAssistant, entity_id: str) -> str:
    """Best display name for an entity."""
    state = hass.states.get(entity_id)
    if state is not None:
        name = state.attributes.get("friendly_name")
        if name:
            return str(name)
    return entity_id


def device_class(hass: HomeAssistant, entity_id: str) -> str | None:
    """Entity device_class if present."""
    state = hass.states.get(entity_id)
    if state is None:
        return None
    dc = state.attributes.get("device_class")
    return str(dc) if dc else None


def area_for_entity(hass: HomeAssistant, entity_id: str) -> str | None:
    """Area id from the entity registry."""
    registry = async_get_entity_registry(hass)
    entry = registry.async_get(entity_id)
    if entry is None:
        return None
    return entry.area_id


def type_options() -> list[dict[str, str]]:
    """Select options for Matter device types."""
    return [{"value": key, "label": label} for key, label in TYPE_LABELS.items()]


def resolve_type(hass: HomeAssistant, entity_id: str, type_key: str | None) -> str:
    """Return a valid type key or raise."""
    domain = domain_from_entity_id(entity_id)
    dc = device_class(hass, entity_id)
    resolved = type_key or default_type_for_entity(entity_id, dc, domain)
    if resolved is None:
        raise HomeAssistantError(
            f"{entity_id} cannot be exported (unsupported domain or sensor class). "
            "Pick a light, switch, cover, or a contact/motion binary sensor."
        )
    if not is_supported_type(resolved):
        raise HomeAssistantError(f"Unsupported type: {resolved}")
    return resolved


async def async_push_catalog(runtime: Any) -> None:
    """Push catalog to bridge; log soft failures."""
    try:
        await runtime.async_push_catalog()
    except Exception as err:  # noqa: BLE001
        _LOGGER.warning("Catalog push failed: %s", err)


async def async_add_entity_export(
    hass: HomeAssistant,
    *,
    entity_id: str,
    name: str | None = None,
    type_key: str | None = None,
    enabled: bool = True,
    linked: dict[str, str] | None = None,
    area_id: str | None = None,
) -> Export:
    """Create one export from an HA entity (UI-friendly defaults)."""
    runtime = get_runtime(hass)
    store: ExportStore = runtime.store

    # Skip duplicate primary entity
    for existing in store.list_exports():
        if existing.primary_entity_id == entity_id:
            raise HomeAssistantError(
                f"{entity_id} is already exported as '{existing.name}'."
            )

    resolved_type = resolve_type(hass, entity_id, type_key)
    export_name = (name or "").strip() or friendly_name(hass, entity_id)
    resolved_area = area_id if area_id is not None else area_for_entity(hass, entity_id)

    try:
        export = Export.new(
            name=export_name,
            type_key=resolved_type,
            primary_entity_id=entity_id,
            linked=dict(linked or {}),
            area_id=resolved_area,
            enabled=enabled,
        )
    except ValueError as err:
        raise HomeAssistantError(str(err)) from err

    await store.async_upsert(export)
    await async_push_catalog(runtime)
    if export.enabled:
        await runtime.async_push_export_state(export)
    runtime.notify_listeners()
    _LOGGER.info("Added export %s for %s", export.export_id, entity_id)
    return export


async def async_add_entities(
    hass: HomeAssistant,
    entity_ids: list[str],
    *,
    type_key: str | None = None,
) -> list[Export]:
    """Add multiple entities as separate exports (bulk UI path)."""
    created: list[Export] = []
    errors: list[str] = []
    for entity_id in entity_ids:
        try:
            exp = await async_add_entity_export(
                hass, entity_id=entity_id, type_key=type_key
            )
            created.append(exp)
        except HomeAssistantError as err:
            errors.append(str(err))
    if not created and errors:
        raise HomeAssistantError("; ".join(errors))
    return created


async def async_update_export_fields(
    hass: HomeAssistant,
    export_id: str,
    *,
    name: str | None = None,
    type_key: str | None = None,
    enabled: bool | None = None,
    primary_entity_id: str | None = None,
    linked: dict[str, str] | None = None,
    area_id: str | None | object = ...,
) -> Export:
    """Patch export fields (only provided ones; area_id=None clears the area)."""
    runtime = get_runtime(hass)
    store: ExportStore = runtime.store
    existing = store.get(export_id)
    if existing is None:
        raise HomeAssistantError("Export not found")

    new_name = (name if name is not None else existing.name).strip()
    new_type = type_key if type_key is not None else existing.type
    new_enabled = enabled if enabled is not None else existing.enabled
    new_primary = (
        primary_entity_id if primary_entity_id is not None else existing.primary_entity_id
    )
    new_linked = dict(linked) if linked is not None else dict(existing.linked)
    new_area = existing.area_id if area_id is ... else area_id

    errors = validate_export_fields(
        name=new_name,
        type_key=new_type,
        primary_entity_id=new_primary,
        linked=new_linked,
    )
    if errors:
        raise HomeAssistantError("; ".join(errors))
    for other in store.list_exports():
        if other.export_id != export_id and other.primary_entity_id == new_primary:
            raise HomeAssistantError(
                f"{new_primary} is already exported as '{other.name}'."
            )

    updated = Export(
        export_id=existing.export_id,
        name=new_name,
        type=new_type,
        primary_entity_id=new_primary,
        linked=new_linked,
        area_id=new_area,
        enabled=new_enabled,
        endpoint_id=existing.endpoint_id,
    )
    await store.async_upsert(updated)
    await async_push_catalog(runtime)
    if updated.enabled:
        await runtime.async_push_export_state(updated)
    runtime.notify_listeners()
    return updated


async def async_remove_export_id(hass: HomeAssistant, export_id: str) -> None:
    """Hard-delete an export."""
    runtime = get_runtime(hass)
    store: ExportStore = runtime.store
    removed = await store.async_delete(export_id)
    if removed is None:
        raise HomeAssistantError("Export not found")
    await async_push_catalog(runtime)
    runtime.notify_listeners()


async def async_reset_export_name(hass: HomeAssistant, export_id: str) -> Export:
    """Copy HA friendly name onto Matter name."""
    runtime = get_runtime(hass)
    store: ExportStore = runtime.store
    existing = store.get(export_id)
    if existing is None:
        raise HomeAssistantError("Export not found")
    # Clamp to the protocol name limit; long HA friendly names must not poison sync.
    existing.name = friendly_name(hass, existing.primary_entity_id)[:64]
    await store.async_upsert(existing)
    await async_push_catalog(runtime)
    runtime.notify_listeners()
    return existing


def export_choice_options(store: ExportStore) -> list[dict[str, str]]:
    """Select options for manage-export UI."""
    options: list[dict[str, str]] = []
    for exp in store.list_exports():
        status = "on" if exp.enabled else "off"
        label = f"{exp.name} ({exp.primary_entity_id}) [{status}]"
        options.append({"value": exp.export_id, "label": label})
    return options
