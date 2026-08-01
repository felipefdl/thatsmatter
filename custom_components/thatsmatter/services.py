"""ThatsMatter services (optional; primary UX is Configure options flow)."""

from __future__ import annotations

import logging
from typing import Any, Awaitable, Callable

import voluptuous as vol

from homeassistant.core import HomeAssistant, ServiceCall, callback
from homeassistant.exceptions import HomeAssistantError, ServiceValidationError
from homeassistant.helpers import config_validation as cv

from .const import (
    ATTR_AREA_ID,
    ATTR_ENABLED,
    ATTR_ENTITY_ID,
    ATTR_EXPORT_ID,
    ATTR_LINKED,
    ATTR_NAME,
    ATTR_PRIMARY_ENTITY_ID,
    ATTR_TYPE,
    DOMAIN,
    SERVICE_ADD_EXPORT,
    SERVICE_REMOVE_EXPORT,
    SERVICE_RESET_NAME_FROM_HA,
    SERVICE_SET_ENABLED,
    SERVICE_UPDATE_EXPORT,
)
from .export_manager import (
    async_add_entity_export,
    async_remove_export_id,
    async_reset_export_name,
    async_update_export_fields,
)

_LOGGER = logging.getLogger(__name__)

ADD_SCHEMA = vol.Schema(
    {
        vol.Required(ATTR_ENTITY_ID): cv.entity_id,
        vol.Optional(ATTR_NAME): cv.string,
        vol.Optional(ATTR_TYPE): cv.string,
        vol.Optional(ATTR_AREA_ID): vol.Any(cv.string, None),
        vol.Optional(ATTR_ENABLED, default=True): cv.boolean,
        vol.Optional(ATTR_LINKED, default=dict): dict,
    }
)

UPDATE_SCHEMA = vol.Schema(
    {
        vol.Required(ATTR_EXPORT_ID): cv.string,
        vol.Optional(ATTR_NAME): cv.string,
        vol.Optional(ATTR_TYPE): cv.string,
        vol.Optional(ATTR_PRIMARY_ENTITY_ID): cv.entity_id,
        vol.Optional(ATTR_AREA_ID): vol.Any(cv.string, None),
        vol.Optional(ATTR_ENABLED): cv.boolean,
        vol.Optional(ATTR_LINKED): dict,
    }
)

REMOVE_SCHEMA = vol.Schema({vol.Required(ATTR_EXPORT_ID): cv.string})
SET_ENABLED_SCHEMA = vol.Schema(
    {
        vol.Required(ATTR_EXPORT_ID): cv.string,
        vol.Required(ATTR_ENABLED): cv.boolean,
    }
)
RESET_NAME_SCHEMA = vol.Schema({vol.Required(ATTR_EXPORT_ID): cv.string})


def _wrap(
    hass: HomeAssistant,
    handler: Callable[[HomeAssistant, ServiceCall], Awaitable[None]],
) -> Callable[[ServiceCall], Awaitable[None]]:
    async def _service(call: ServiceCall) -> None:
        try:
            await handler(hass, call)
        except HomeAssistantError as err:
            raise ServiceValidationError(str(err)) from err

    return _service


async def async_add_export(hass: HomeAssistant, call: ServiceCall) -> None:
    """Service wrapper for add export."""
    await async_add_entity_export(
        hass,
        entity_id=call.data[ATTR_ENTITY_ID],
        name=call.data.get(ATTR_NAME),
        type_key=call.data.get(ATTR_TYPE),
        enabled=call.data.get(ATTR_ENABLED, True),
        linked=call.data.get(ATTR_LINKED),
        area_id=call.data.get(ATTR_AREA_ID),
    )


async def async_update_export(hass: HomeAssistant, call: ServiceCall) -> None:
    """Service wrapper for update export."""
    kwargs: dict[str, Any] = {}
    if ATTR_NAME in call.data:
        kwargs["name"] = call.data[ATTR_NAME]
    if ATTR_TYPE in call.data:
        kwargs["type_key"] = call.data[ATTR_TYPE]
    if ATTR_ENABLED in call.data:
        kwargs["enabled"] = call.data[ATTR_ENABLED]
    if ATTR_PRIMARY_ENTITY_ID in call.data:
        kwargs["primary_entity_id"] = call.data[ATTR_PRIMARY_ENTITY_ID]
    if ATTR_LINKED in call.data:
        kwargs["linked"] = call.data[ATTR_LINKED]
    if ATTR_AREA_ID in call.data:
        kwargs["area_id"] = call.data[ATTR_AREA_ID]
    await async_update_export_fields(hass, call.data[ATTR_EXPORT_ID], **kwargs)


async def async_remove_export(hass: HomeAssistant, call: ServiceCall) -> None:
    """Service wrapper for remove export."""
    await async_remove_export_id(hass, call.data[ATTR_EXPORT_ID])


async def async_set_enabled(hass: HomeAssistant, call: ServiceCall) -> None:
    """Service wrapper for enable/disable."""
    await async_update_export_fields(
        hass,
        call.data[ATTR_EXPORT_ID],
        enabled=call.data[ATTR_ENABLED],
    )


async def async_reset_name_from_ha(hass: HomeAssistant, call: ServiceCall) -> None:
    """Service wrapper for reset name."""
    await async_reset_export_name(hass, call.data[ATTR_EXPORT_ID])


@callback
def async_register_services(hass: HomeAssistant) -> None:
    """Register domain services once (automation power users; UI is primary)."""
    if hass.data.get(f"_{DOMAIN}_services"):
        return
    hass.data[f"_{DOMAIN}_services"] = True

    hass.services.async_register(
        DOMAIN, SERVICE_ADD_EXPORT, _wrap(hass, async_add_export), schema=ADD_SCHEMA
    )
    hass.services.async_register(
        DOMAIN,
        SERVICE_UPDATE_EXPORT,
        _wrap(hass, async_update_export),
        schema=UPDATE_SCHEMA,
    )
    hass.services.async_register(
        DOMAIN,
        SERVICE_REMOVE_EXPORT,
        _wrap(hass, async_remove_export),
        schema=REMOVE_SCHEMA,
    )
    hass.services.async_register(
        DOMAIN,
        SERVICE_SET_ENABLED,
        _wrap(hass, async_set_enabled),
        schema=SET_ENABLED_SCHEMA,
    )
    hass.services.async_register(
        DOMAIN,
        SERVICE_RESET_NAME_FROM_HA,
        _wrap(hass, async_reset_name_from_ha),
        schema=RESET_NAME_SCHEMA,
    )


@callback
def async_unregister_services(hass: HomeAssistant) -> None:
    """Remove domain services (called when the last config entry unloads)."""
    if not hass.data.pop(f"_{DOMAIN}_services", None):
        return
    for service in (
        SERVICE_ADD_EXPORT,
        SERVICE_UPDATE_EXPORT,
        SERVICE_REMOVE_EXPORT,
        SERVICE_SET_ENABLED,
        SERVICE_RESET_NAME_FROM_HA,
    ):
        hass.services.async_remove(DOMAIN, service)
