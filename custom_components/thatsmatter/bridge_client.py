"""HTTP client for the ThatsMatter bridge loopback IPC API."""

from __future__ import annotations

import json
from typing import Any

from aiohttp import ClientError, ClientSession, ClientTimeout

from .helpers import create_export_body


class BridgeClientError(Exception):
    """Raised when the bridge returns a non-success response or is unreachable."""

    def __init__(
        self,
        message: str,
        *,
        status: int | None = None,
        body: Any = None,
    ) -> None:
        super().__init__(message)
        self.status = status
        self.body = body


class BridgeClient:
    """Async client for bridge control-plane endpoints (protocol/schema.json)."""

    def __init__(
        self,
        host: str,
        port: int,
        session: ClientSession,
        *,
        timeout: float = 10.0,
    ) -> None:
        self._base = f"http://{host}:{port}"
        self._session = session
        self._timeout = ClientTimeout(total=timeout)

    @property
    def base_url(self) -> str:
        """Base URL of the bridge IPC server."""
        return self._base

    async def _request(
        self,
        method: str,
        path: str,
        *,
        json_body: dict[str, Any] | list[Any] | None = None,
    ) -> Any:
        url = f"{self._base}{path}"
        try:
            async with self._session.request(
                method,
                url,
                json=json_body,
                timeout=self._timeout,
            ) as resp:
                text = await resp.text()
                body: Any = None
                if text:
                    try:
                        body = json.loads(text)
                    except json.JSONDecodeError:
                        body = text
                if resp.status >= 400:
                    message = (
                        body.get("message", text)
                        if isinstance(body, dict)
                        else (text or f"HTTP {resp.status}")
                    )
                    raise BridgeClientError(
                        str(message),
                        status=resp.status,
                        body=body,
                    )
                return body
        except BridgeClientError:
            raise
        except ClientError as err:
            raise BridgeClientError(f"bridge unreachable: {err}") from err
        except TimeoutError as err:
            raise BridgeClientError("bridge request timed out") from err

    async def health(self) -> dict[str, Any]:
        """GET /health."""
        result = await self._request("GET", "/health")
        return result if isinstance(result, dict) else {}

    async def status(self) -> dict[str, Any]:
        """GET /status."""
        result = await self._request("GET", "/status")
        return result if isinstance(result, dict) else {}

    async def pairing(self) -> dict[str, Any]:
        """GET /pairing."""
        result = await self._request("GET", "/pairing")
        return result if isinstance(result, dict) else {}

    async def open_pairing(self, timeout_secs: int = 300) -> dict[str, Any]:
        """POST /pairing/open with optional timeout (bridge clamps 180..=900)."""
        result = await self._request(
            "POST",
            "/pairing/open",
            json_body={"timeout_secs": int(timeout_secs)},
        )
        return result if isinstance(result, dict) else {}

    async def close_pairing(self) -> dict[str, Any]:
        """POST /pairing/close."""
        result = await self._request("POST", "/pairing/close")
        return result if isinstance(result, dict) else {}

    async def list_exports(self) -> list[dict[str, Any]]:
        """GET /exports."""
        result = await self._request("GET", "/exports")
        if not isinstance(result, list):
            return []
        return result

    async def get_export(self, export_id: str) -> dict[str, Any]:
        """GET /exports/{id}."""
        result = await self._request("GET", f"/exports/{export_id}")
        return result if isinstance(result, dict) else {}

    async def create_export(self, export: dict[str, Any]) -> dict[str, Any]:
        """POST /exports with optional export_id."""
        body = create_export_body(export)
        result = await self._request("POST", "/exports", json_body=body)
        return result if isinstance(result, dict) else {}

    async def patch_export(
        self, export_id: str, patch: dict[str, Any]
    ) -> dict[str, Any]:
        """PATCH /exports/{id}."""
        result = await self._request(
            "PATCH", f"/exports/{export_id}", json_body=patch
        )
        return result if isinstance(result, dict) else {}

    async def delete_export(self, export_id: str) -> dict[str, Any]:
        """DELETE /exports/{id}."""
        result = await self._request("DELETE", f"/exports/{export_id}")
        return result if isinstance(result, dict) else {}

    async def push_state(
        self,
        export_id: str,
        states: list[dict[str, Any]] | dict[str, Any],
    ) -> dict[str, Any]:
        """POST /exports/{id}/state with HaStateUpdate or HaStateValue."""
        if isinstance(states, dict):
            body: dict[str, Any] | list[Any] = states
        else:
            body = {"states": states}
        result = await self._request(
            "POST", f"/exports/{export_id}/state", json_body=body
        )
        return result if isinstance(result, dict) else {}

    async def take_commands(self) -> list[dict[str, Any]]:
        """GET /commands (drains pending Matter -> HA commands)."""
        result = await self._request("GET", "/commands")
        if not isinstance(result, dict):
            return []
        commands = result.get("commands") or []
        return list(commands) if isinstance(commands, list) else []

    async def sync_catalog(self, exports: list[dict[str, Any]]) -> list[dict[str, Any]]:
        """Reconcile HA catalog onto the bridge via create/patch/delete.

        HA is source of truth. Returns the bridge export list after sync.
        """
        remote = await self.list_exports()
        remote_by_id = {str(item["export_id"]): item for item in remote}
        local_by_id = {str(item["export_id"]): item for item in exports}

        # Delete remote-only exports.
        for rid in list(remote_by_id):
            if rid not in local_by_id:
                await self.delete_export(rid)

        result: list[dict[str, Any]] = []
        for eid, local in local_by_id.items():
            if eid not in remote_by_id:
                created = await self.create_export(local)
                result.append(created)
                continue
            remote_item = remote_by_id[eid]
            patch: dict[str, Any] = {}
            if remote_item.get("name") != local.get("name"):
                patch["name"] = local["name"]
            if remote_item.get("type") != local.get("type"):
                patch["type"] = local["type"]
            if remote_item.get("primary_entity_id") != local.get("primary_entity_id"):
                patch["primary_entity_id"] = local["primary_entity_id"]
            if (remote_item.get("linked") or {}) != (local.get("linked") or {}):
                patch["linked"] = local.get("linked") or {}
            if remote_item.get("area_id") != local.get("area_id"):
                patch["area_id"] = local.get("area_id")
            if bool(remote_item.get("enabled", True)) != bool(
                local.get("enabled", True)
            ):
                patch["enabled"] = bool(local.get("enabled", True))
            if patch:
                updated = await self.patch_export(eid, patch)
                result.append(updated)
            else:
                result.append(remote_item)
        return result
