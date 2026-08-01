"""Runtime coordinator: catalog sync, HA state push, command poll loop."""

from __future__ import annotations

import asyncio
import logging
from typing import Any

from homeassistant.const import (
    ATTR_ENTITY_ID,
    SERVICE_CLOSE_COVER,
    SERVICE_OPEN_COVER,
    SERVICE_SET_COVER_POSITION,
    SERVICE_STOP_COVER,
    SERVICE_TURN_OFF,
    SERVICE_TURN_ON,
)
from homeassistant.core import Event, HomeAssistant, callback
from homeassistant.helpers.aiohttp_client import async_get_clientsession

from .bridge_client import BridgeClient, BridgeClientError
from .const import COMMAND_POLL_INTERVAL, DOMAIN, STATUS_POLL_INTERVAL
from .helpers import ha_state_value, matter_level_to_ha_brightness
from .models import Export
from .store import ExportStore

_LOGGER = logging.getLogger(__name__)


class ThatsMatterRuntime:
    """Per-config-entry runtime: store, bridge client, listeners, poll loop."""

    def __init__(
        self,
        hass: HomeAssistant,
        *,
        host: str,
        port: int,
        bridge_name: str,
        store: ExportStore,
    ) -> None:
        self.hass = hass
        self.host = host
        self.port = port
        self.bridge_name = bridge_name
        self.store = store
        self.client: BridgeClient | None = None
        self.bridge_connected = False
        self.bridge_status: dict[str, Any] = {}
        self.pairing: dict[str, Any] = {}
        self.last_error: str | None = None
        self._unsub_state: Any = None
        self._command_task: asyncio.Task[None] | None = None
        self._status_task: asyncio.Task[None] | None = None
        self._started = False
        self._listeners: list[Any] = []
        self._pairing_notice_id = f"{DOMAIN}_pairing"
        self._last_notified_code: str | None = None

    async def async_start(self) -> None:
        """Connect to external bridge, push catalog, start loops."""
        if self._started:
            return
        session = async_get_clientsession(self.hass)
        self.client = BridgeClient(self.host, self.port, session)
        await self.store.async_load()
        # Prefer config-entry bridge name if store is still default empty name.
        if self.store.bridge_name != self.bridge_name and not self.store.list_exports():
            await self.store.async_set_bridge_name(self.bridge_name)

        try:
            await self.client.health()
            self.bridge_connected = True
            self.last_error = None
        except BridgeClientError as err:
            self.bridge_connected = False
            self.last_error = str(err)
            _LOGGER.warning(
                "ThatsMatter bridge not reachable at %s:%s (%s); "
                "will keep retrying. Assume the external bridge is running.",
                self.host,
                self.port,
                err,
            )

        if self.bridge_connected:
            try:
                await self.async_push_catalog()
                await self.async_refresh_status()
                await self.async_refresh_pairing()
                await self.async_push_all_states()
                await self.async_show_pairing_notification()
            except BridgeClientError as err:
                # Loops below retry; a transient failure must not fail entry setup.
                self.bridge_connected = False
                self.last_error = str(err)
                _LOGGER.warning("Initial bridge sync failed (%s); will keep retrying", err)

        self._subscribe_states()
        self._command_task = self.hass.async_create_background_task(
            self._command_loop(),
            name=f"{DOMAIN}_command_loop",
        )
        self._status_task = self.hass.async_create_background_task(
            self._status_loop(),
            name=f"{DOMAIN}_status_loop",
        )
        self._started = True

    def add_listener(self, listener: Any) -> None:
        """Register a callback for catalog/pairing updates."""
        if listener not in self._listeners:
            self._listeners.append(listener)

    def remove_listener(self, listener: Any) -> None:
        """Unregister a runtime listener."""
        if listener in self._listeners:
            self._listeners.remove(listener)

    def notify_listeners(self) -> None:
        """Notify entity listeners (UI refresh)."""
        for listener in list(self._listeners):
            try:
                listener()
            except Exception:  # noqa: BLE001
                _LOGGER.exception("ThatsMatter listener failed")

    async def async_show_pairing_notification(self) -> None:
        """Surface pairing code in the HA notification drawer (no YAML / host shell)."""
        code = self.pairing.get("setup_code")
        if not code:
            return
        code_s = str(code)
        if code_s == self._last_notified_code:
            return
        self._last_notified_code = code_s
        message = (
            f"**ThatsMatter is ready to pair**\n\n"
            f"Setup code: `{code_s}`\n\n"
            f"In Alexa, Google Home, SmartThings, or Apple Home: "
            f"**Add device → Matter**, then enter this code (or open "
            f"**Settings → Devices & services → ThatsMatter → Configure → Pair with other apps** "
            f"to see the QR code).\n\n"
            f"Nothing is shared until you add devices under **Configure → Add devices**."
        )
        from homeassistant.components.persistent_notification import (
            async_create as async_create_notification,
        )

        async_create_notification(
            self.hass,
            message,
            title="ThatsMatter pairing",
            notification_id=self._pairing_notice_id,
        )

    async def async_stop(self) -> None:
        """Cancel loops and listeners."""
        self._started = False
        if self._unsub_state is not None:
            self._unsub_state()
            self._unsub_state = None
        self._listeners.clear()
        for task in (self._command_task, self._status_task):
            if task is not None and not task.done():
                task.cancel()
                try:
                    await task
                except asyncio.CancelledError:
                    pass
        self._command_task = None
        self._status_task = None

    def _require_client(self) -> BridgeClient:
        if self.client is None:
            raise BridgeClientError("bridge client not started")
        return self.client

    async def async_push_catalog(self) -> None:
        """Forward HA export catalog to the bridge."""
        client = self._require_client()
        exports = [e.to_protocol_dict() for e in self.store.list_exports()]
        try:
            remote = await client.sync_catalog(exports)
            self.bridge_connected = True
            self.last_error = None
            if self.store.apply_endpoint_ids(remote):
                await self.store.async_save()
            _LOGGER.debug("Pushed %s exports to bridge", len(exports))
        except BridgeClientError as err:
            self.bridge_connected = False
            self.last_error = str(err)
            _LOGGER.warning("Failed to push catalog: %s", err)
            raise

    async def async_refresh_status(self) -> None:
        """Refresh bridge status snapshot."""
        client = self._require_client()
        try:
            self.bridge_status = await client.status()
            self.bridge_connected = True
            self.last_error = None
        except BridgeClientError as err:
            self.bridge_connected = False
            self.last_error = str(err)
            self.bridge_status = {}

    async def async_refresh_pairing(self) -> None:
        """Refresh pairing material for UI entities."""
        client = self._require_client()
        try:
            self.pairing = await client.pairing()
            self.notify_listeners()
        except BridgeClientError as err:
            _LOGGER.debug("Pairing refresh failed: %s", err)
            self.pairing = {}

    async def async_push_all_states(self) -> None:
        """Push current HA state for every enabled export."""
        for exp in self.store.list_exports():
            if not exp.enabled:
                continue
            await self.async_push_export_state(exp)

    async def async_push_export_state(self, export: Export) -> None:
        """Push HA states for one export to the bridge."""
        client = self._require_client()
        entity_ids = [export.primary_entity_id, *export.linked.values()]
        states: list[dict[str, Any]] = []
        for entity_id in entity_ids:
            state = self.hass.states.get(entity_id)
            if state is None:
                continue
            states.append(
                ha_state_value(
                    entity_id,
                    state.state,
                    dict(state.attributes),
                )
            )
        if not states:
            return
        try:
            await client.push_state(export.export_id, states)
        except BridgeClientError as err:
            _LOGGER.debug(
                "State push failed for %s: %s", export.export_id, err
            )

    def _subscribe_states(self) -> None:
        """Listen for state_changed; handler filters to enabled export entities."""
        if self._unsub_state is not None:
            self._unsub_state()
            self._unsub_state = None

        @callback
        def _on_state(event: Event) -> None:
            # Filter before creating a task; this runs for every state_changed in HA.
            entity_id = event.data.get("entity_id")
            if not entity_id or not self.store.data.find_by_entity(str(entity_id)):
                return
            self.hass.async_create_task(self._async_handle_state_event(event))

        # Bus-wide listen so catalog growth does not require re-subscribe.
        self._unsub_state = self.hass.bus.async_listen(
            "state_changed",
            _on_state,
        )

    async def _async_handle_state_event(self, event: Event) -> None:
        entity_id = event.data.get("entity_id")
        if not entity_id:
            return
        matches = self.store.data.find_by_entity(str(entity_id))
        if not matches:
            return
        if not self.bridge_connected or self.client is None:
            return
        for exp in matches:
            await self.async_push_export_state(exp)

    async def _command_loop(self) -> None:
        """Poll bridge commands and execute HA services."""
        while self._started:
            try:
                if self.client is not None:
                    was_connected = self.bridge_connected
                    commands = await self.client.take_commands()
                    self.bridge_connected = True
                    self.last_error = None
                    if not was_connected:
                        # This loop polls faster than the status loop, so it is
                        # usually the first to observe a reconnect.
                        await self._async_on_reconnected()
                    for cmd in commands:
                        await self._async_execute_command(cmd)
            except asyncio.CancelledError:
                raise
            except BridgeClientError as err:
                self.bridge_connected = False
                self.last_error = str(err)
            except Exception:  # noqa: BLE001
                _LOGGER.exception("Command loop error")
            await asyncio.sleep(COMMAND_POLL_INTERVAL)

    async def _async_on_reconnected(self) -> None:
        """Bridge came (back) up: re-push catalog and entity states."""
        _LOGGER.info(
            "Bridge reachable at %s:%s; syncing catalog", self.host, self.port
        )
        await self._async_resync()

    async def _async_resync(self) -> None:
        """Re-push catalog and entity states after the bridge (re)connects."""
        try:
            await self.async_push_catalog()
            await self.async_push_all_states()
        except BridgeClientError:
            return

    async def _status_loop(self) -> None:
        """Periodically refresh status and pairing material."""
        while self._started:
            try:
                if self.client is not None:
                    was_connected = self.bridge_connected
                    await self.async_refresh_status()
                    if self.bridge_connected and not was_connected:
                        await self._async_on_reconnected()
                    await self.async_refresh_pairing()
                    await self.async_show_pairing_notification()
            except asyncio.CancelledError:
                raise
            except Exception:  # noqa: BLE001
                _LOGGER.debug("Status loop error", exc_info=True)
            await asyncio.sleep(STATUS_POLL_INTERVAL)

    async def _async_execute_command(self, cmd: dict[str, Any]) -> None:
        export_id = str(cmd.get("export_id", ""))
        kind = cmd.get("kind")
        export = self.store.get(export_id)
        if export is None or not export.enabled:
            _LOGGER.debug("Ignoring command for missing/disabled export %s", export_id)
            return

        entity_id = export.primary_entity_id
        domain = entity_id.split(".", 1)[0]
        data: dict[str, Any] = {ATTR_ENTITY_ID: entity_id}

        try:
            if kind == "on_off":
                on = bool(cmd.get("on", True))
                if domain == "cover":
                    service = SERVICE_OPEN_COVER if on else SERVICE_CLOSE_COVER
                else:
                    service = SERVICE_TURN_ON if on else SERVICE_TURN_OFF
                await self.hass.services.async_call(
                    domain, service, data, blocking=True
                )
            elif kind == "level":
                level = int(cmd.get("level") or 0)
                brightness = matter_level_to_ha_brightness(level)
                if brightness <= 0:
                    await self.hass.services.async_call(
                        domain, SERVICE_TURN_OFF, data, blocking=True
                    )
                else:
                    data["brightness"] = brightness
                    await self.hass.services.async_call(
                        "light", SERVICE_TURN_ON, data, blocking=True
                    )
            elif kind == "cover_position":
                position = int(cmd.get("position") or 0)
                data["position"] = max(0, min(100, position))
                await self.hass.services.async_call(
                    "cover", SERVICE_SET_COVER_POSITION, data, blocking=True
                )
            elif kind == "cover_open":
                await self.hass.services.async_call(
                    "cover", SERVICE_OPEN_COVER, data, blocking=True
                )
            elif kind == "cover_close":
                await self.hass.services.async_call(
                    "cover", SERVICE_CLOSE_COVER, data, blocking=True
                )
            elif kind == "cover_stop":
                await self.hass.services.async_call(
                    "cover", SERVICE_STOP_COVER, data, blocking=True
                )
            else:
                _LOGGER.warning("Unknown command kind: %s", kind)
        except Exception:  # noqa: BLE001
            _LOGGER.exception(
                "Failed to execute command %s for export %s", kind, export_id
            )
