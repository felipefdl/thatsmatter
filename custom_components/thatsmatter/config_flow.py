"""Config and options flows for ThatsMatter (UI only, no YAML)."""

from __future__ import annotations

from typing import Any

import voluptuous as vol

from homeassistant import config_entries
from homeassistant.core import HomeAssistant, callback
from homeassistant.data_entry_flow import FlowResult
from homeassistant.exceptions import HomeAssistantError
from homeassistant.helpers.aiohttp_client import async_get_clientsession
from homeassistant.helpers.selector import (
    EntitySelector,
    EntitySelectorConfig,
    SelectSelector,
    SelectSelectorConfig,
    SelectOptionDict,
    TextSelector,
    TextSelectorConfig,
)
from homeassistant.helpers.service_info.hassio import HassioServiceInfo

from .bridge_client import BridgeClient, BridgeClientError
from .const import (
    CONF_BRIDGE_HOST,
    CONF_BRIDGE_NAME,
    CONF_BRIDGE_PORT,
    DEFAULT_BRIDGE_HOST,
    DEFAULT_BRIDGE_NAME,
    DEFAULT_BRIDGE_PORT,
    DOMAIN,
)
from .export_manager import (
    async_add_entities,
    async_remove_export_id,
    async_reset_export_name,
    async_update_export_fields,
    export_choice_options,
    get_runtime,
    type_options,
)
from .helpers import SUPPORTED_DOMAINS

async def validate_input(hass: HomeAssistant, data: dict[str, Any]) -> dict[str, Any]:
    """Validate bridge host/port/name and return entry fields."""
    host = data[CONF_BRIDGE_HOST].strip()
    port = int(data[CONF_BRIDGE_PORT])
    name = data[CONF_BRIDGE_NAME].strip() or DEFAULT_BRIDGE_NAME

    if not host:
        raise InvalidHost
    if port < 1 or port > 65535:
        raise InvalidHost
    if len(name) > 32:
        raise InvalidHost

    session = async_get_clientsession(hass)
    client = BridgeClient(host, port, session, timeout=3.0)
    try:
        await client.health()
    except BridgeClientError:
        pass

    return {
        "title": name,
        CONF_BRIDGE_HOST: host,
        CONF_BRIDGE_PORT: port,
        CONF_BRIDGE_NAME: name,
    }


class ThatsMatterConfigFlow(config_entries.ConfigFlow, domain=DOMAIN):
    """Handle a config flow for ThatsMatter."""

    VERSION = 1

    def __init__(self) -> None:
        """Initialize flow state."""
        self._discovery: dict[str, Any] = {}

    async def async_step_user(
        self, user_input: dict[str, Any] | None = None
    ) -> FlowResult:
        """Handle the initial setup step."""
        errors: dict[str, str] = {}

        if user_input is not None:
            try:
                info = await validate_input(self.hass, user_input)
            except InvalidHost:
                errors["base"] = "invalid_host"
            except Exception:  # noqa: BLE001
                errors["base"] = "unknown"
            else:
                await self.async_set_unique_id(DOMAIN)
                self._abort_if_unique_id_configured()
                return self.async_create_entry(
                    title=info["title"],
                    data={
                        CONF_BRIDGE_HOST: info[CONF_BRIDGE_HOST],
                        CONF_BRIDGE_PORT: info[CONF_BRIDGE_PORT],
                        CONF_BRIDGE_NAME: info[CONF_BRIDGE_NAME],
                    },
                )

        defaults = {
            CONF_BRIDGE_HOST: self._discovery.get(CONF_BRIDGE_HOST, DEFAULT_BRIDGE_HOST),
            CONF_BRIDGE_PORT: self._discovery.get(CONF_BRIDGE_PORT, DEFAULT_BRIDGE_PORT),
            CONF_BRIDGE_NAME: self._discovery.get(CONF_BRIDGE_NAME, DEFAULT_BRIDGE_NAME),
        }
        schema = vol.Schema(
            {
                vol.Required(CONF_BRIDGE_HOST, default=defaults[CONF_BRIDGE_HOST]): str,
                vol.Required(CONF_BRIDGE_PORT, default=defaults[CONF_BRIDGE_PORT]): int,
                vol.Required(CONF_BRIDGE_NAME, default=defaults[CONF_BRIDGE_NAME]): str,
            }
        )
        return self.async_show_form(
            step_id="user",
            data_schema=schema,
            errors=errors,
        )

    async def async_step_hassio(self, discovery_info: HassioServiceInfo) -> FlowResult:
        """Handle discovery from the ThatsMatter App (add-on)."""
        config = discovery_info.config or {}
        host = str(config.get("host") or DEFAULT_BRIDGE_HOST)
        port = int(config.get("port") or DEFAULT_BRIDGE_PORT)
        name = str(config.get("bridge_name") or config.get("name") or DEFAULT_BRIDGE_NAME)

        await self.async_set_unique_id(DOMAIN)
        self._abort_if_unique_id_configured(
            updates={
                CONF_BRIDGE_HOST: host,
                CONF_BRIDGE_PORT: port,
                CONF_BRIDGE_NAME: name,
            }
        )

        self._discovery = {
            CONF_BRIDGE_HOST: host,
            CONF_BRIDGE_PORT: port,
            CONF_BRIDGE_NAME: name,
        }
        self.context["title_placeholders"] = {"name": name}
        return await self.async_step_hassio_confirm()

    async def async_step_hassio_confirm(
        self, user_input: dict[str, Any] | None = None
    ) -> FlowResult:
        """Confirm App discovery and create the config entry."""
        if user_input is not None:
            data = {
                CONF_BRIDGE_HOST: self._discovery.get(CONF_BRIDGE_HOST, DEFAULT_BRIDGE_HOST),
                CONF_BRIDGE_PORT: self._discovery.get(CONF_BRIDGE_PORT, DEFAULT_BRIDGE_PORT),
                CONF_BRIDGE_NAME: self._discovery.get(CONF_BRIDGE_NAME, DEFAULT_BRIDGE_NAME),
            }
            try:
                info = await validate_input(self.hass, data)
            except InvalidHost:
                return self.async_abort(reason="invalid_host")
            return self.async_create_entry(
                title=info["title"],
                data={
                    CONF_BRIDGE_HOST: info[CONF_BRIDGE_HOST],
                    CONF_BRIDGE_PORT: info[CONF_BRIDGE_PORT],
                    CONF_BRIDGE_NAME: info[CONF_BRIDGE_NAME],
                },
            )

        return self.async_show_form(
            step_id="hassio_confirm",
            description_placeholders={
                "host": str(self._discovery.get(CONF_BRIDGE_HOST, DEFAULT_BRIDGE_HOST)),
                "port": str(self._discovery.get(CONF_BRIDGE_PORT, DEFAULT_BRIDGE_PORT)),
                "name": str(self._discovery.get(CONF_BRIDGE_NAME, DEFAULT_BRIDGE_NAME)),
            },
        )

    @staticmethod
    @callback
    def async_get_options_flow(
        config_entry: config_entries.ConfigEntry,
    ) -> config_entries.OptionsFlow:
        """Return the options flow (main user UI for catalog + pairing)."""
        return ThatsMatterOptionsFlow()


class ThatsMatterOptionsFlow(config_entries.OptionsFlow):
    """UI to pair controllers and manage exports without YAML or services."""

    def __init__(self) -> None:
        """Init options flow state."""
        self._selected_export_id: str | None = None

    async def async_step_init(
        self, user_input: dict[str, Any] | None = None
    ) -> FlowResult:
        """Main menu."""
        return self.async_show_menu(
            step_id="init",
            menu_options=["pairing", "add_export", "manage_exports", "connection"],
        )

    async def async_step_pairing(
        self, user_input: dict[str, Any] | None = None
    ) -> FlowResult:
        """Open the pairing window, then show setup code and QR guidance."""
        if user_input is not None:
            return await self.async_step_init()

        code = "—"
        qr = "—"
        try:
            runtime = get_runtime(self.hass)
            # Ensure the shown code is usable for multi-admin pairing.
            try:
                await runtime.async_open_pairing_window()
            except BridgeClientError:
                # Bridge offline or open failed; still try last-known material.
                await runtime.async_refresh_pairing()
            code = str(runtime.pairing.get("setup_code") or "—")
            qr = str(runtime.pairing.get("qr_payload") or "—")
        except HomeAssistantError:
            pass

        return self.async_show_form(
            step_id="pairing",
            data_schema=vol.Schema({}),
            description_placeholders={
                "setup_code": code,
                "qr_payload": qr,
            },
        )

    async def async_step_add_export(
        self, user_input: dict[str, Any] | None = None
    ) -> FlowResult:
        """Pick one or more HA entities in the UI and export them."""
        errors: dict[str, str] = {}

        if user_input is not None:
            entity_ids = user_input.get("entities") or []
            if isinstance(entity_ids, str):
                entity_ids = [entity_ids]
            type_key = user_input.get("type") or None
            if type_key == "auto":
                type_key = None
            try:
                created = await async_add_entities(
                    self.hass, list(entity_ids), type_key=type_key
                )
            except HomeAssistantError as err:
                errors["base"] = "add_failed"
                self.context["add_error"] = str(err)
            else:
                self.context["add_names"] = ", ".join(e.name for e in created) or "none"
                self.context["add_count"] = str(len(created))
                return await self.async_step_add_done()

        type_opts = [SelectOptionDict(value="auto", label="Automatic (from entity)")]
        type_opts.extend(
            SelectOptionDict(value=o["value"], label=o["label"]) for o in type_options()
        )

        schema = vol.Schema(
            {
                vol.Required("entities"): EntitySelector(
                    EntitySelectorConfig(
                        domain=list(SUPPORTED_DOMAINS),
                        multiple=True,
                    )
                ),
                vol.Optional("type", default="auto"): SelectSelector(
                    SelectSelectorConfig(options=type_opts, mode="dropdown")
                ),
            }
        )
        placeholders = {}
        if errors:
            placeholders["error_detail"] = self.context.get("add_error", "")
        return self.async_show_form(
            step_id="add_export",
            data_schema=schema,
            errors=errors,
            description_placeholders=placeholders or {"error_detail": ""},
        )

    async def async_step_add_done(
        self, user_input: dict[str, Any] | None = None
    ) -> FlowResult:
        """Confirmation after adding exports."""
        if user_input is not None:
            return await self.async_step_init()
        return self.async_show_form(
            step_id="add_done",
            data_schema=vol.Schema({}),
            description_placeholders={
                "names": self.context.get("add_names", ""),
                "count": self.context.get("add_count", "0"),
            },
        )

    async def async_step_manage_exports(
        self, user_input: dict[str, Any] | None = None
    ) -> FlowResult:
        """Pick an export to edit, disable, or remove."""
        errors: dict[str, str] = {}
        try:
            runtime = get_runtime(self.hass)
            options = export_choice_options(runtime.store)
        except HomeAssistantError:
            return self.async_abort(reason="not_configured")

        if not options:
            return self.async_show_form(
                step_id="manage_empty",
                data_schema=vol.Schema({}),
            )

        if user_input is not None:
            self._selected_export_id = user_input["export_id"]
            return await self.async_step_edit_export()

        select_opts = [
            SelectOptionDict(value=o["value"], label=o["label"]) for o in options
        ]
        schema = vol.Schema(
            {
                vol.Required("export_id"): SelectSelector(
                    SelectSelectorConfig(options=select_opts, mode="dropdown")
                )
            }
        )
        return self.async_show_form(
            step_id="manage_exports",
            data_schema=schema,
            errors=errors,
        )

    async def async_step_manage_empty(
        self, user_input: dict[str, Any] | None = None
    ) -> FlowResult:
        """No exports yet."""
        if user_input is not None:
            return await self.async_step_add_export()
        return self.async_show_form(
            step_id="manage_empty",
            data_schema=vol.Schema({}),
        )

    async def async_step_edit_export(
        self, user_input: dict[str, Any] | None = None
    ) -> FlowResult:
        """Edit name, type, enabled; or remove / reset name."""
        errors: dict[str, str] = {}
        export_id = self._selected_export_id
        if not export_id:
            return await self.async_step_manage_exports()

        try:
            runtime = get_runtime(self.hass)
            exp = runtime.store.get(export_id)
        except HomeAssistantError:
            return self.async_abort(reason="not_configured")
        if exp is None:
            return await self.async_step_manage_exports()

        if user_input is not None:
            action = user_input.get("action", "save")
            try:
                if action == "remove":
                    await async_remove_export_id(self.hass, export_id)
                    return await self.async_step_manage_exports()
                if action == "reset_name":
                    await async_reset_export_name(self.hass, export_id)
                    return await self.async_step_edit_export()
                await async_update_export_fields(
                    self.hass,
                    export_id,
                    name=user_input.get("name"),
                    type_key=user_input.get("type"),
                    enabled=user_input.get("enabled"),
                )
                return await self.async_step_manage_exports()
            except HomeAssistantError:
                errors["base"] = "update_failed"

        # Reload after reset_name
        exp = runtime.store.get(export_id) or exp
        type_opts = [
            SelectOptionDict(value=o["value"], label=o["label"]) for o in type_options()
        ]
        action_opts = [
            SelectOptionDict(value="save", label="Save changes"),
            SelectOptionDict(value="reset_name", label="Reset name from Home Assistant"),
            SelectOptionDict(value="remove", label="Remove from Matter (delete)"),
        ]
        schema = vol.Schema(
            {
                vol.Required("name", default=exp.name): TextSelector(
                    TextSelectorConfig(type="text")
                ),
                vol.Required("type", default=exp.type): SelectSelector(
                    SelectSelectorConfig(options=type_opts, mode="dropdown")
                ),
                vol.Required("enabled", default=exp.enabled): bool,
                vol.Required("action", default="save"): SelectSelector(
                    SelectSelectorConfig(options=action_opts, mode="dropdown")
                ),
            }
        )
        return self.async_show_form(
            step_id="edit_export",
            data_schema=schema,
            errors=errors,
            description_placeholders={
                "entity_id": exp.primary_entity_id,
                "export_id": exp.export_id,
            },
        )

    async def async_step_connection(
        self, user_input: dict[str, Any] | None = None
    ) -> FlowResult:
        """Edit bridge host/port/name (stored as options)."""
        if user_input is not None:
            return self.async_create_entry(title="", data=user_input)

        data = {**self.config_entry.data, **self.config_entry.options}
        schema = vol.Schema(
            {
                vol.Required(
                    CONF_BRIDGE_HOST,
                    default=data.get(CONF_BRIDGE_HOST, DEFAULT_BRIDGE_HOST),
                ): str,
                vol.Required(
                    CONF_BRIDGE_PORT,
                    default=data.get(CONF_BRIDGE_PORT, DEFAULT_BRIDGE_PORT),
                ): int,
                vol.Required(
                    CONF_BRIDGE_NAME,
                    default=data.get(CONF_BRIDGE_NAME, DEFAULT_BRIDGE_NAME),
                ): str,
            }
        )
        return self.async_show_form(step_id="connection", data_schema=schema)


class InvalidHost(HomeAssistantError):
    """Invalid host, port, or bridge name."""
